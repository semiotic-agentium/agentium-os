//! Tool function registration system
//!
//! This module provides a trait-based system for registering tool functions
//! that can be called by LLMs during BAML function execution or directly from JavaScript.

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    marker::PhantomData,
    pin::Pin,
    sync::{Arc, Mutex as StdMutex},
};

use async_trait::async_trait;
use baml_rt_core::{BamlRtError, ContextId, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex as TokioMutex;

use crate::{
    bundles::BundleType,
    tool_catalog::{InventoryCatalog, ToolCatalog},
    tool_fsm::{SessionPhase, ToolFailure, ToolSession, ToolSessionError, ToolSessionId, ToolStep},
    tool_schema::{ToolType, json_schema_value},
    ts_gen::render_tool_typescript,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ToolAccess {
    Read,
    Write,
    Delete,
}

impl std::fmt::Display for ToolAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            ToolAccess::Read => "read",
            ToolAccess::Write => "write",
            ToolAccess::Delete => "delete",
        };
        write!(f, "{}", value)
    }
}

/// Capitalize the first character of a string
///
/// Used for generating class names and TypeScript identifiers.
pub(crate) fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// Helper function for creating an empty open_input value.
///
/// This centralizes the pattern of using an empty JSON object as the default
/// open_input when none is provided.
fn empty_open_input() -> Value {
    serde_json::Value::Object(serde_json::Map::new())
}

/// Deserialize a tool input from JSON Value
///
/// Centralizes error handling for tool input deserialization.
fn deserialize_tool_input<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(BamlRtError::Json)
}

/// Serialize a tool output to JSON Value
///
/// Centralizes error handling for tool output serialization.
fn serialize_tool_output<T: Serialize>(value: T) -> Result<Value> {
    serde_json::to_value(value).map_err(BamlRtError::Json)
}

fn tool_registry_trace_enabled() -> bool {
    std::env::var("BAML_TRACE_TOOL_SESSION").is_ok()
}

fn tool_registry_trace(message: &str) {
    if tool_registry_trace_enabled() {
        eprintln!("[tool-registry-trace] {}", message);
    }
}

/// Validate open_input by attempting to deserialize it
///
/// Centralizes the validation pattern for open_input parameters.
/// For unit type `()`, both `null` and empty object `{}` are accepted (registry uses `{}` for one-shot execute).
pub fn validate_open_input<T: for<'de> Deserialize<'de>>(open_input: Value) -> Result<()> {
    match serde_json::from_value::<T>(open_input.clone()) {
        Ok(_) => Ok(()),
        Err(err) => {
            let msg = err.to_string();
            if msg.contains("expected unit")
                && (open_input.is_null()
                    || open_input
                        .as_object()
                        .map(|m| m.is_empty())
                        .unwrap_or(false))
            {
                Ok(())
            } else {
                Err(BamlRtError::InvalidOpenInput { source: err })
            }
        }
    }
}

/// Parse a tool name and derive its class name
///
/// Centralizes the common pattern of parsing a tool name string
/// and deriving the corresponding class name.
pub fn parse_tool_name_and_class(name: &str) -> Result<(ToolName, String)> {
    let parsed = ToolName::parse(name)?;
    let class_name = ToolFunctionMetadata::derive_class_name(parsed.bundle(), parsed.local());
    Ok((parsed, class_name))
}

/// Trait for BAML tools that can be called by LLMs or JavaScript
///
/// Tools implement this trait to provide:
/// - Name and metadata
/// - Input schema for LLM understanding
/// - Execution logic
///
/// # Example
/// ```rust,no_run
/// use baml_rt_tools::{BamlTool, Support};
/// use baml_rt_core::Result;
/// use serde::{Deserialize, Serialize};
/// use schemars::JsonSchema;
/// use ts_rs::TS;
/// use async_trait::async_trait;
///
/// struct WeatherTool;
///
/// #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
/// #[ts(export)]
/// struct WeatherInput {
///     location: String,
/// }
///
/// #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
/// #[ts(export)]
/// struct WeatherOutput {
///     temperature: String,
///     location: String,
/// }
///
/// #[async_trait]
/// impl BamlTool for WeatherTool {
///     type Bundle = Support;
///     const LOCAL_NAME: &'static str = "get_weather";
///     type OpenInput = ();
///     type Input = WeatherInput;
///     type Output = WeatherOutput;
///
///     fn description(&self) -> &'static str {
///         "Gets the current weather for a specific location"
///     }
///
///     async fn execute(&self, args: Self::Input) -> Result<Self::Output> {
///         Ok(WeatherOutput {
///             temperature: "22°C".to_string(),
///             location: args.location,
///         })
///     }
/// }
/// ```
#[async_trait]
pub trait BamlTool: Send + Sync + 'static {
    /// The bundle type this tool belongs to (e.g., `Support`)
    type Bundle: crate::bundles::BundleType;

    /// The local name of this tool (e.g., "calculate", "get_weather")
    /// The full tool name will be derived as "{Bundle::NAME}/{LOCAL_NAME}"
    const LOCAL_NAME: &'static str;

    /// Typed input for opening the session (initial_input in Open step)
    /// Use `()` for tools that don't need args when opening
    type OpenInput: ToolType + Serialize + for<'de> Deserialize<'de>;

    /// Typed input for sending to an open session (input in Send step)
    type Input: ToolType + Serialize + for<'de> Deserialize<'de>;

    /// Typed output for this tool
    type Output: ToolType + Serialize;

    /// The unique qualified name of this tool (derived from Bundle::NAME and LOCAL_NAME)
    fn name() -> String {
        format!("{}/{}", Self::Bundle::NAME, Self::LOCAL_NAME)
    }

    /// The class name for BAML generation (e.g., "SupportCalculate" from Support + Calculate)
    fn class_name() -> String {
        let bundle_name = Self::Bundle::NAME;
        let local_name = Self::LOCAL_NAME;
        format!(
            "{}{}",
            capitalize_first(bundle_name),
            capitalize_first(local_name)
        )
    }

    /// Description of what this tool does (used by LLMs to understand when to call it)
    fn description(&self) -> &'static str;

    /// JSON schema describing the tool's open input parameters
    fn open_input_schema(&self) -> Value {
        json_schema_value::<Self::OpenInput>()
    }

    /// JSON schema describing the tool's input parameters
    fn input_schema(&self) -> Value {
        json_schema_value::<Self::Input>()
    }

    /// JSON schema describing the tool's output
    fn output_schema(&self) -> Value {
        json_schema_value::<Self::Output>()
    }

    /// Execute the tool with the given arguments
    ///
    /// # Arguments
    /// * `args` - Typed input for the tool
    ///
    /// # Returns
    /// Typed output for the tool
    async fn execute(&self, args: Self::Input) -> Result<Self::Output>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSecretRequirement {
    pub name: String,
    pub description: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolTypeSpec {
    pub name: String,
    pub ts_decl: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BundleName(String);

impl BundleName {
    pub fn new(name: impl Into<String>) -> Result<Self> {
        let name = name.into();
        if name.is_empty() || name.contains('/') {
            return Err(BamlRtError::InvalidArgument(format!(
                "Bundle name '{}' must be non-empty and must not contain '/'",
                name
            )));
        }
        Ok(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for BundleName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for BundleName {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for BundleName {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let name = String::deserialize(deserializer)?;
        BundleName::new(name).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<String> for BundleName {
    type Error = BamlRtError;

    fn try_from(value: String) -> Result<Self> {
        BundleName::new(value)
    }
}

impl From<BundleName> for String {
    fn from(value: BundleName) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LocalToolName(String);

impl LocalToolName {
    pub fn new(name: impl Into<String>) -> Result<Self> {
        let name = name.into();
        if name.is_empty() || name.contains('/') {
            return Err(BamlRtError::InvalidArgument(format!(
                "Tool name '{}' must be non-empty and must not contain '/'",
                name
            )));
        }
        Ok(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for LocalToolName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for LocalToolName {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for LocalToolName {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let name = String::deserialize(deserializer)?;
        LocalToolName::new(name).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<String> for LocalToolName {
    type Error = BamlRtError;

    fn try_from(value: String) -> Result<Self> {
        LocalToolName::new(value)
    }
}

impl From<LocalToolName> for String {
    fn from(value: LocalToolName) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ToolName {
    bundle: BundleName,
    local: LocalToolName,
}

impl ToolName {
    pub fn parse(name: &str) -> Result<Self> {
        let parts: Vec<&str> = name.split('/').collect();
        if parts.len() != 2 {
            return Err(BamlRtError::InvalidArgument(format!(
                "Tool name '{}' must be formatted as interface/tool",
                name
            )));
        }
        Ok(Self {
            bundle: BundleName::new(parts[0].to_string())?,
            local: LocalToolName::new(parts[1].to_string())?,
        })
    }

    pub fn qualified(bundle: BundleName, local: LocalToolName) -> Self {
        Self { bundle, local }
    }

    pub fn bundle(&self) -> &BundleName {
        &self.bundle
    }

    pub fn local(&self) -> &LocalToolName {
        &self.local
    }
}

impl std::fmt::Display for ToolName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.bundle, self.local)
    }
}

impl Serialize for ToolName {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ToolName {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let name = String::deserialize(deserializer)?;
        ToolName::parse(&name).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<String> for ToolName {
    type Error = BamlRtError;

    fn try_from(value: String) -> Result<Self> {
        ToolName::parse(&value)
    }
}

impl From<ToolName> for String {
    fn from(value: ToolName) -> Self {
        value.to_string()
    }
}

/// Metadata describing a tool function
#[derive(Debug, Clone)]
pub struct ToolFunctionMetadata {
    /// Tool name (must be unique)
    pub name: ToolName,
    /// Class name for BAML generation (e.g., "SupportCalculate")
    pub class_name: String,
    /// Tool description (used by LLMs to understand what the tool does)
    pub description: String,
    /// JSON schema for the tool's open input parameters (initial_input in Open step)
    pub open_input_schema: Value,
    /// JSON schema for the tool's input parameters (input in Send step)
    pub input_schema: Value,
    /// JSON schema for the tool's output
    pub output_schema: Value,
    /// Open input type metadata
    pub open_input_type: ToolTypeSpec,
    /// Input type metadata
    pub input_type: ToolTypeSpec,
    /// Output type metadata
    pub output_type: ToolTypeSpec,
    /// Pre-rendered BAML type declarations from `BamlType::baml_decl()`.
    ///
    /// When present, `baml_gen.rs` uses this directly instead of converting
    /// JSON schemas via `schema_to_baml`. Populated by tools that derive
    /// `BamlType`; `None` for JS/guest tools that rely on the JSON Schema path.
    pub baml_decl: Option<String>,
    /// Extra TypeScript declarations required by tool types (e.g. dependent enums).
    pub extra_ts_decls: Vec<String>,
    /// Access level required to invoke this tool.
    pub access: Option<ToolAccess>,
    /// Tool tags for indexing/search
    pub tags: Vec<String>,
    /// Secrets required to execute this tool
    pub secret_requirements: Vec<ToolSecretRequirement>,
    /// Origin of this tool (host vs guest)
    pub origin: ToolOrigin,
}

/// Trait for building ToolFunctionMetadata consistently
///
/// Provides a consistent interface for constructing tool metadata,
/// ensuring all metadata follows the same pattern and reducing duplication.
pub trait ToolMetadataBuilder {
    /// Build ToolFunctionMetadata from this builder
    fn build_metadata(self) -> ToolFunctionMetadata;
}

impl ToolFunctionMetadata {
    pub fn bundle(&self) -> &BundleName {
        self.name.bundle()
    }

    /// Derive class name from bundle and local tool names
    pub fn derive_class_name(bundle: &BundleName, local: &LocalToolName) -> String {
        format!(
            "{}{}",
            capitalize_first(bundle.as_str()),
            capitalize_first(local.as_str())
        )
    }

    /// Create ToolFunctionMetadata from type parameters
    ///
    /// This helper consolidates the common pattern of building metadata
    /// from type information, reducing duplication across registration sites.
    #[allow(clippy::too_many_arguments)] // prefer TypeBasedMetadataBuilder for new call sites
    pub fn from_types<OpenInput, Input, Output>(
        name: ToolName,
        class_name: String,
        description: String,
        tags: Vec<String>,
        secret_requirements: Vec<ToolSecretRequirement>,
        origin: ToolOrigin,
        extra_ts_decls: Vec<String>,
        access: Option<ToolAccess>,
    ) -> Self
    where
        OpenInput: crate::tool_schema::ToolType,
        Input: crate::tool_schema::ToolType,
        Output: crate::tool_schema::ToolType,
    {
        use crate::tool_schema::{json_schema_value, ts_decl, ts_name};
        Self {
            name,
            class_name,
            description,
            open_input_schema: json_schema_value::<OpenInput>(),
            input_schema: json_schema_value::<Input>(),
            output_schema: json_schema_value::<Output>(),
            open_input_type: ToolTypeSpec {
                name: ts_name::<OpenInput>(),
                ts_decl: ts_decl::<OpenInput>(),
            },
            input_type: ToolTypeSpec {
                name: ts_name::<Input>(),
                ts_decl: ts_decl::<Input>(),
            },
            output_type: ToolTypeSpec {
                name: ts_name::<Output>(),
                ts_decl: ts_decl::<Output>(),
            },
            baml_decl: None,
            extra_ts_decls,
            access,
            tags,
            secret_requirements,
            origin,
        }
    }
}

/// Builder for constructing ToolFunctionMetadata from type parameters
pub struct TypeBasedMetadataBuilder<OpenInput, Input, Output> {
    name: ToolName,
    class_name: String,
    description: String,
    baml_decl: Option<String>,
    tags: Vec<String>,
    secret_requirements: Vec<ToolSecretRequirement>,
    origin: ToolOrigin,
    extra_ts_decls: Vec<String>,
    access: Option<ToolAccess>,
    _phantom: std::marker::PhantomData<(OpenInput, Input, Output)>,
}

impl<OpenInput, Input, Output> TypeBasedMetadataBuilder<OpenInput, Input, Output>
where
    OpenInput: crate::tool_schema::ToolType,
    Input: crate::tool_schema::ToolType,
    Output: crate::tool_schema::ToolType,
{
    /// Create a new builder with required fields
    pub fn new(name: ToolName, class_name: String, description: String) -> Self {
        Self {
            name,
            class_name,
            description,
            baml_decl: None,
            extra_ts_decls: Vec::new(),
            access: None,
            tags: Vec::new(),
            secret_requirements: Vec::new(),
            origin: ToolOrigin::Host,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Set pre-rendered BAML type declarations from `BamlType::baml_decl()`.
    ///
    /// When set, `baml_gen.rs` emits this directly instead of converting
    /// JSON schemas via `schema_to_baml`.
    pub fn with_baml_decl(mut self, decl: String) -> Self {
        self.baml_decl = Some(decl);
        self
    }

    /// Set tags for the tool
    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    /// Set secret requirements for the tool
    pub fn with_secrets(mut self, secrets: Vec<ToolSecretRequirement>) -> Self {
        self.secret_requirements = secrets;
        self
    }

    /// Set the tool origin
    pub fn with_origin(mut self, origin: ToolOrigin) -> Self {
        self.origin = origin;
        self
    }

    /// Add extra TypeScript declarations required by tool types.
    pub fn with_extra_ts_decls(mut self, extra_ts_decls: Vec<String>) -> Self {
        self.extra_ts_decls = extra_ts_decls;
        self
    }

    /// Set access level for this tool.
    pub fn with_access(mut self, access: ToolAccess) -> Self {
        self.access = Some(access);
        self
    }
}

impl<OpenInput, Input, Output> ToolMetadataBuilder
    for TypeBasedMetadataBuilder<OpenInput, Input, Output>
where
    OpenInput: crate::tool_schema::ToolType,
    Input: crate::tool_schema::ToolType,
    Output: crate::tool_schema::ToolType,
{
    fn build_metadata(self) -> ToolFunctionMetadata {
        let mut metadata = ToolFunctionMetadata::from_types::<OpenInput, Input, Output>(
            self.name,
            self.class_name,
            self.description,
            self.tags,
            self.secret_requirements,
            self.origin,
            self.extra_ts_decls,
            self.access,
        );
        metadata.baml_decl = self.baml_decl;
        metadata
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunctionMetadataExport {
    pub name: ToolName,
    pub class_name: String,
    pub description: String,
    pub open_input_schema: Value,
    pub input_schema: Value,
    pub output_schema: Value,
    pub open_input_type: ToolTypeSpec,
    pub input_type: ToolTypeSpec,
    pub output_type: ToolTypeSpec,
    pub baml_decl: Option<String>,
    pub extra_ts_decls: Vec<String>,
    pub access: Option<ToolAccess>,
    pub tags: Vec<String>,
    pub secret_requirements: Vec<ToolSecretRequirement>,
    pub origin: ToolOrigin,
}

impl From<&ToolFunctionMetadata> for ToolFunctionMetadataExport {
    fn from(metadata: &ToolFunctionMetadata) -> Self {
        Self {
            name: metadata.name.clone(),
            class_name: metadata.class_name.clone(),
            description: metadata.description.clone(),
            open_input_schema: metadata.open_input_schema.clone(),
            input_schema: metadata.input_schema.clone(),
            output_schema: metadata.output_schema.clone(),
            open_input_type: metadata.open_input_type.clone(),
            input_type: metadata.input_type.clone(),
            output_type: metadata.output_type.clone(),
            baml_decl: metadata.baml_decl.clone(),
            extra_ts_decls: metadata.extra_ts_decls.clone(),
            access: metadata.access,
            tags: metadata.tags.clone(),
            secret_requirements: metadata.secret_requirements.clone(),
            origin: metadata.origin,
        }
    }
}

/// Discovery record for tool search (name, bundle, description, tags, access, origin).
/// Discovery lists globally available tools only; no per-agent invokability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDiscoveryRecord {
    pub name: ToolName,
    pub bundle: BundleName,
    pub description: String,
    pub tags: Vec<String>,
    pub access: Option<ToolAccess>,
    pub origin: ToolOrigin,
}

impl ToolDiscoveryRecord {
    pub fn from_metadata(metadata: &ToolFunctionMetadata) -> Self {
        Self {
            name: metadata.name.clone(),
            bundle: metadata.name.bundle().clone(),
            description: metadata.description.clone(),
            tags: metadata.tags.clone(),
            access: metadata.access,
            origin: metadata.origin,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolBundleMetadata {
    pub name: BundleName,
    pub description: String,
    pub config_schema: Option<Value>,
    pub secret_requirements: Vec<ToolSecretRequirement>,
}

/// Declares whether this tool ever emits `ToolStep::Streaming` from `next()`.
///
/// The session protocol: after `send(input)`, the caller calls `next()` until the step indicates
/// completion. Each `next()` returns a `ToolStep`: `Streaming { output }` (more to come),
/// `Done { output }` (completion), or `Error { error }`.
///
/// - **OneShot:** This tool only ever returns `Done` or `Error` from `next()`. One `next()` after
///   a `send()` returns the full result and signals completion. `ToolRegistry::execute()` (one
///   open → send → next → finish) is allowed.
/// - **Streaming:** This tool may return `ToolStep::Streaming` one or more times. Each `next()` may
///   block or buffer and does *not* indicate completion until the step is `Done`. Callers must
///   use `open_session` and call `next()` in a loop until `Done`. `execute()` is disabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCapability {
    OneShot,
    Streaming,
}

/// Origin of a tool invocation (host vs guest)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolOrigin {
    /// Tool is a host tool (manifest allowlist applies)
    Host,
    /// Tool is a guest tool (no allowlist restriction)
    Guest,
}

/// Context passed to a tool when a session is opened. context_id (from invocation scope)
/// is set by the executor so tools (e.g. internal_a2a) can attach it to outbound requests for
/// session continuity (e.g. INPUT_REQUIRED resume).
pub struct ToolSessionContext {
    pub session_id: ToolSessionId,
    pub tool_name: ToolName,
    /// Invocation scope context_id; used by internal_a2a for delegated session continuity.
    pub context_id: ContextId,
}

#[async_trait]
pub trait ToolHandler: Send + Sync {
    fn metadata(&self) -> &ToolFunctionMetadata;
    fn capability(&self) -> ToolCapability {
        ToolCapability::OneShot
    }
    async fn open_session(
        &self,
        ctx: ToolSessionContext,
        open_input: Value,
    ) -> Result<Box<dyn ToolSession>>;
}

pub trait ToolBundle: Send + Sync {
    fn metadata(&self) -> ToolBundleMetadata;
    fn functions(&self) -> Vec<Arc<dyn ToolHandler>>;
}

/// Registry for dynamically registered tool functions
pub struct ToolRegistry {
    inner: StdMutex<ToolRegistryInner>,
}

struct ToolRegistryInner {
    tools: HashMap<ToolName, (ToolFunctionMetadata, Arc<dyn ToolHandler>)>,
    bundles: HashMap<BundleName, ToolBundleMetadata>,
    allowlist: Option<HashSet<ToolName>>,
    sessions: HashMap<ToolSessionId, Arc<TokioMutex<Box<dyn ToolSession>>>>,
}

fn map_session_error(error: ToolSessionError) -> BamlRtError {
    match error {
        ToolSessionError::Transport(err) => err,
        ToolSessionError::Tool(failure) => BamlRtError::InvalidArgument(format!(
            "Tool failure ({:?}): {}",
            failure.kind, failure.message
        )),
    }
}

pub struct AwaitingInput;
pub struct Ready;
pub struct Closed;

/// Trait marker for session state - indicates whether the session is closed
pub trait SessionState {
    /// Whether this state represents a closed session
    const IS_CLOSED: bool;
}

impl SessionState for AwaitingInput {
    const IS_CLOSED: bool = false;
}

impl SessionState for Ready {
    const IS_CLOSED: bool = false;
}

impl SessionState for Closed {
    const IS_CLOSED: bool = true;
}

pub enum ToolSessionAdvance {
    Streaming {
        output: Value,
        session: ToolSessionHandle<Ready>,
    },
    /// Session yielded output but is suspended (e.g. input required). Session remains open; do not call finish.
    Suspended {
        output: Value,
        session: ToolSessionHandle<Ready>,
    },
    Done {
        output: Option<Value>,
        session: ToolSessionHandle<Closed>,
    },
    Error {
        error: ToolFailure,
        session: ToolSessionHandle<Closed>,
    },
}

pub struct ToolSessionHandle<State: SessionState> {
    id: ToolSessionId,
    registry: Arc<ToolRegistry>,
    _state: PhantomData<State>,
}

impl ToolSessionHandle<AwaitingInput> {
    pub async fn open(
        registry: Arc<ToolRegistry>,
        name: &str,
        open_input: Value,
        context_id: &ContextId,
    ) -> Result<ToolSessionHandle<AwaitingInput>> {
        let session_id = registry.open_session(name, open_input, context_id).await?;
        Ok(ToolSessionHandle {
            id: session_id,
            registry,
            _state: PhantomData,
        })
    }

    pub fn session_id(&self) -> &ToolSessionId {
        &self.id
    }

    pub async fn send(self, input: Value) -> Result<ToolSessionHandle<Ready>> {
        let registry = self.registry.clone();
        let id = self.id.clone();
        registry.session_send(&id, input).await?;
        Ok(ToolSessionHandle {
            id,
            registry,
            _state: PhantomData,
        })
    }
}

impl ToolSessionHandle<Ready> {
    pub fn session_id(&self) -> &ToolSessionId {
        &self.id
    }

    pub async fn next(self) -> Result<ToolSessionAdvance> {
        let registry = self.registry.clone();
        let registry_handle = self.registry.clone();
        let id = self.id.clone();
        let step = {
            let step = registry.session_next(&id).await?;
            match &step {
                ToolStep::Done { .. } => {
                    registry.session_finish(&id).await?;
                }
                ToolStep::Error { error } => {
                    registry
                        .session_abort(&id, Some(error.message.clone()))
                        .await?;
                }
                ToolStep::Streaming { .. } | ToolStep::Suspended { .. } => {}
            }
            step
        };
        match step {
            ToolStep::Streaming { output } => Ok(ToolSessionAdvance::Streaming {
                output,
                session: ToolSessionHandle {
                    id,
                    registry: registry_handle,
                    _state: PhantomData,
                },
            }),
            ToolStep::Suspended { output } => Ok(ToolSessionAdvance::Suspended {
                output,
                session: ToolSessionHandle {
                    id,
                    registry: registry_handle,
                    _state: PhantomData,
                },
            }),
            ToolStep::Done { output } => Ok(ToolSessionAdvance::Done {
                output,
                session: ToolSessionHandle {
                    id,
                    registry: registry_handle,
                    _state: PhantomData,
                },
            }),
            ToolStep::Error { error } => Ok(ToolSessionAdvance::Error {
                error,
                session: ToolSessionHandle {
                    id,
                    registry: registry_handle,
                    _state: PhantomData,
                },
            }),
        }
    }

    pub async fn finish(self) -> Result<ToolSessionHandle<Closed>> {
        let registry = self.registry.clone();
        let id = self.id.clone();
        registry.session_finish(&id).await?;
        Ok(ToolSessionHandle {
            id,
            registry,
            _state: PhantomData,
        })
    }

    pub async fn abort(self, reason: Option<String>) -> Result<ToolSessionHandle<Closed>> {
        let registry = self.registry.clone();
        let id = self.id.clone();
        registry.session_abort(&id, reason).await?;
        Ok(ToolSessionHandle {
            id,
            registry,
            _state: PhantomData,
        })
    }
}

impl ToolSessionHandle<Closed> {
    pub fn session_id(&self) -> &ToolSessionId {
        &self.id
    }
}

impl<State: SessionState> Drop for ToolSessionHandle<State> {
    fn drop(&mut self) {
        // Only abort if the session is not already closed
        if State::IS_CLOSED {
            return;
        }
        let registry = self.registry.clone();
        let session_id = self.id.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let reason = "session dropped";
                let span = crate::spans::session_abort(&session_id, Some(reason));
                let _guard = span.enter();

                if let Err(e) = registry
                    .session_abort(&session_id, Some(reason.to_string()))
                    .await
                {
                    tracing::warn!(
                        session_id = %session_id,
                        error = ?e,
                        "Failed to abort session during drop"
                    );
                }
            });
        }
    }
}

/// Internal trait for executing tools (bridges trait objects to async trait)
/// This provides a unified execution interface that can be used by sessions
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn execute(&self, args: Value) -> Result<Value>;
}

/// Adapter that implements ToolExecutor for any function-like handler
struct ExecutorAdapter<F> {
    executor: Arc<F>,
}

impl<F> ExecutorAdapter<F>
where
    F: Fn(Value) -> Pin<Box<dyn Future<Output = Result<Value>> + Send>> + Send + Sync + 'static,
{
    fn new(executor: F) -> Self {
        Self {
            executor: Arc::new(executor),
        }
    }
}

#[async_trait]
impl<F> ToolExecutor for ExecutorAdapter<F>
where
    F: Fn(Value) -> Pin<Box<dyn Future<Output = Result<Value>> + Send>> + Send + Sync + 'static,
{
    async fn execute(&self, args: Value) -> Result<Value> {
        (self.executor)(args).await
    }
}

/// Wrapper that implements ToolHandler for any BamlTool
pub(crate) struct ToolWrapper<T: BamlTool> {
    pub(crate) tool: Arc<T>,
    pub(crate) metadata: ToolFunctionMetadata,
}

/// Build metadata and handler from a BamlTool (type-level metadata only).
/// Used by the single tool-provider inventory for host registration.
pub fn create_tool_handler<T: BamlTool>(
    tool: T,
) -> Result<(ToolFunctionMetadata, Arc<dyn ToolHandler>)> {
    let name = ToolName::parse(&T::name())?;
    let expected_bundle = T::Bundle::bundle_name()?;
    if name.bundle() != &expected_bundle {
        return Err(BamlRtError::InvalidArgument(format!(
            "Tool '{}' bundle '{}' does not match Bundle type '{}'",
            name,
            name.bundle(),
            expected_bundle
        )));
    }
    let description_str = tool.description().to_string();
    let metadata = TypeBasedMetadataBuilder::<T::OpenInput, T::Input, T::Output>::new(
        name.clone(),
        T::class_name(),
        description_str.clone(),
    )
    .build_metadata();
    let handler: Arc<dyn ToolHandler> = Arc::new(ToolWrapper {
        tool: Arc::new(tool),
        metadata: metadata.clone(),
    });
    Ok((metadata, handler))
}

#[async_trait]
impl<T: BamlTool> ToolHandler for ToolWrapper<T> {
    fn metadata(&self) -> &ToolFunctionMetadata {
        &self.metadata
    }

    async fn open_session(
        &self,
        ctx: ToolSessionContext,
        open_input: Value,
    ) -> Result<Box<dyn ToolSession>> {
        // Parse and validate open_input if needed
        validate_open_input::<T::OpenInput>(open_input)?;

        let tool = self.tool.clone();
        let executor: Box<dyn ToolExecutor> = Box::new(ExecutorAdapter::new(move |input| {
            let tool = tool.clone();
            Box::pin(async move {
                let parsed: T::Input = deserialize_tool_input(input)?;
                let output = tool.execute(parsed).await?;
                serialize_tool_output(output)
            })
        }));
        Ok(Box::new(OneShotSession::new(ctx, executor)))
    }
}

struct MultiSendSession {
    /// Reserved for session-scoped tracing/abort; not yet used. In production, allow(dead_code) is a smell—revisit when adding session-scoped ops.
    #[allow(dead_code)]
    ctx: ToolSessionContext,
    executor: Box<dyn ToolExecutor>,
    pending: Option<Value>,
}

impl MultiSendSession {
    fn new(ctx: ToolSessionContext, executor: Box<dyn ToolExecutor>) -> Self {
        Self {
            ctx,
            executor,
            pending: None,
        }
    }
}

/// Session that allows multiple Send/Next pairs. Each Send runs the executor and stores the
/// result; Next returns that result (and clears it so the next Next returns None until next Send).
#[async_trait]
impl ToolSession for MultiSendSession {
    async fn send(&mut self, input: Value) -> std::result::Result<(), ToolSessionError> {
        let output = match self.executor.execute(input).await {
            Ok(value) => value,
            Err(err) => {
                return Err(ToolSessionError::Tool(ToolFailure::from_error(&err)));
            }
        };
        self.pending = Some(output);
        Ok(())
    }

    async fn next(&mut self) -> std::result::Result<ToolStep, ToolSessionError> {
        let output = self.pending.take();
        Ok(ToolStep::Done {
            output: output.map(Some).unwrap_or(None),
        })
    }

    async fn finish(&mut self) -> std::result::Result<(), ToolSessionError> {
        Ok(())
    }

    async fn abort(
        &mut self,
        _reason: Option<String>,
    ) -> std::result::Result<(), ToolSessionError> {
        Ok(())
    }
}

impl ToolRegistry {
    /// Create a new empty tool registry
    pub fn new() -> Self {
        Self {
            inner: StdMutex::new(ToolRegistryInner {
                tools: HashMap::new(),
                bundles: HashMap::new(),
                allowlist: None,
                sessions: HashMap::new(),
            }),
        }
    }

    pub fn set_allowlist(&self, allowlist: HashSet<ToolName>) {
        let mut inner = self.inner.lock().unwrap();
        inner.allowlist = Some(allowlist);
    }

    pub fn set_allowlist_from_strings(&self, allowlist: HashSet<String>) -> Result<()> {
        let mut parsed = HashSet::with_capacity(allowlist.len());
        for name in allowlist {
            parsed.insert(ToolName::parse(&name)?);
        }
        let mut inner = self.inner.lock().unwrap();
        inner.allowlist = Some(parsed);
        Ok(())
    }

    pub fn clear_allowlist(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.allowlist = None;
    }

    /// Register a tool that implements the BamlTool trait
    ///
    /// # Arguments
    /// * `tool` - An instance of a type implementing `BamlTool`
    ///
    /// # Example
    /// ```rust,no_run
    /// use baml_rt_tools::{ToolRegistry, BamlTool, Support};
    /// use baml_rt_core::Result;
    /// use serde::{Deserialize, Serialize};
    /// use schemars::JsonSchema;
    /// use ts_rs::TS;
    /// use async_trait::async_trait;
    ///
    /// struct MyTool;
    ///
    /// #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
    /// #[ts(export)]
    /// struct MyInput {}
    ///
    /// #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
    /// #[ts(export)]
    /// struct MyOutput {}
    ///
    /// #[async_trait]
    /// impl BamlTool for MyTool {
    ///     type Bundle = Support;
    ///     const LOCAL_NAME: &'static str = "my_tool";
    ///     type OpenInput = ();
    ///     type Input = MyInput;
    ///     type Output = MyOutput;
    ///     fn description(&self) -> &'static str { "My tool" }
    ///     async fn execute(&self, _args: Self::Input) -> Result<Self::Output> {
    ///         Ok(MyOutput {})
    ///     }
    /// }
    ///
    /// let registry = ToolRegistry::new();
    /// registry.register(MyTool).expect("register tool");
    /// ```
    pub fn register<T: BamlTool>(&self, tool: T) -> Result<()> {
        let (metadata, tool_handler) = create_tool_handler(tool)?;
        let name = metadata.name.clone();
        let description_str = metadata.description.clone();

        let mut inner = self.inner.lock().unwrap();
        if let Some(allowlist) = &inner.allowlist
            && !allowlist.contains(&name)
        {
            return Err(BamlRtError::InvalidArgument(format!(
                "Tool '{}' is not declared in the manifest allowlist",
                name
            )));
        }

        if inner.tools.contains_key(&name) {
            return Err(BamlRtError::InvalidArgument(format!(
                "Tool '{}' is already registered",
                name
            )));
        }

        inner
            .tools
            .insert(name.clone(), (metadata.clone(), tool_handler));

        let span = crate::spans::register_tool(&name, &description_str);
        let _guard = span.enter();
        crate::metrics::record_tool_registration(&name.to_string());
        tracing::info!(
            tool = %name,
            description = description_str.as_str(),
            "Registered tool function"
        );

        Ok(())
    }

    /// Register a tool with dynamic metadata and handler.
    pub fn register_dynamic(
        &self,
        metadata: ToolFunctionMetadata,
        handler: Arc<dyn ToolHandler>,
    ) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        if metadata.origin == ToolOrigin::Host
            && let Some(allowlist) = &inner.allowlist
            && !allowlist.contains(&metadata.name)
        {
            return Err(BamlRtError::InvalidArgument(format!(
                "Tool '{}' is not declared in the manifest allowlist",
                metadata.name
            )));
        }

        if inner.tools.contains_key(&metadata.name) {
            return Err(BamlRtError::InvalidArgument(format!(
                "Tool '{}' is already registered",
                metadata.name
            )));
        }

        tracing::info!(
            tool = %metadata.name,
            description = metadata.description.as_str(),
            "Registered dynamic tool function"
        );

        inner
            .tools
            .insert(metadata.name.clone(), (metadata, handler));

        Ok(())
    }

    pub fn register_bundle<T: ToolBundle>(&self, bundle: T) -> Result<()> {
        let bundle_meta = bundle.metadata();
        let mut inner = self.inner.lock().unwrap();
        if inner.bundles.contains_key(&bundle_meta.name) {
            return Err(BamlRtError::InvalidArgument(format!(
                "Bundle '{}' is already registered",
                bundle_meta.name
            )));
        }
        for handler in bundle.functions() {
            let metadata = handler.metadata().clone();
            if metadata.name.bundle() != &bundle_meta.name {
                return Err(BamlRtError::InvalidArgument(format!(
                    "Tool '{}' does not match bundle '{}'",
                    metadata.name, bundle_meta.name
                )));
            }
            if metadata.origin == ToolOrigin::Host
                && let Some(allowlist) = &inner.allowlist
                && !allowlist.contains(&metadata.name)
            {
                return Err(BamlRtError::InvalidArgument(format!(
                    "Tool '{}' is not declared in the manifest allowlist",
                    metadata.name
                )));
            }
            if inner.tools.contains_key(&metadata.name) {
                return Err(BamlRtError::InvalidArgument(format!(
                    "Tool '{}' is already registered",
                    metadata.name
                )));
            }
            inner
                .tools
                .insert(metadata.name.clone(), (metadata, handler.clone()));
        }
        inner.bundles.insert(bundle_meta.name.clone(), bundle_meta);
        Ok(())
    }

    /// Get tool metadata by name
    pub fn get_metadata(&self, name: &str) -> Option<ToolFunctionMetadata> {
        let parsed = ToolName::parse(name).ok()?;
        let inner = self.inner.lock().unwrap();
        inner
            .tools
            .get(&parsed)
            .map(|(metadata, _)| metadata.clone())
    }

    /// List all registered tool names
    pub fn list_tools(&self) -> Vec<String> {
        let inner = self.inner.lock().unwrap();
        inner.tools.keys().map(|name| name.to_string()).collect()
    }

    /// Get all tool metadata (for LLM function calling)
    pub fn all_metadata(&self) -> Vec<ToolFunctionMetadata> {
        let inner = self.inner.lock().unwrap();
        inner
            .tools
            .values()
            .map(|(metadata, _)| metadata.clone())
            .collect()
    }

    pub fn export_metadata(&self) -> Vec<ToolFunctionMetadata> {
        let inner = self.inner.lock().unwrap();
        inner
            .tools
            .values()
            .filter(|(metadata, _)| metadata.origin == ToolOrigin::Host)
            .map(|(metadata, _)| metadata.clone())
            .collect()
    }

    pub fn export_metadata_records(&self) -> Vec<ToolFunctionMetadataExport> {
        let inner = self.inner.lock().unwrap();
        inner
            .tools
            .values()
            .filter(|(metadata, _)| metadata.origin == ToolOrigin::Host)
            .map(|(metadata, _)| ToolFunctionMetadataExport::from(metadata))
            .collect()
    }

    /// Search the **whole** tool catalog (all tools the host knows about from inventory). Discovery is global.
    pub fn search_tools(&self, query: &str, limit: usize) -> Vec<ToolDiscoveryRecord> {
        let full_catalog = crate::tool_catalog::all_tool_metadata();
        crate::tool_discovery::search_tools(&full_catalog, query, limit)
    }

    pub fn validate_allowlist_registered(&self) -> Result<()> {
        let inner = self.inner.lock().unwrap();
        if let Some(allowlist) = &inner.allowlist {
            let mut missing = Vec::new();
            for name in allowlist {
                if !inner.tools.contains_key(name) {
                    missing.push(name.to_string());
                }
            }
            if !missing.is_empty() {
                return Err(BamlRtError::InvalidArgument(format!(
                    "Manifest tools missing from registry: {}",
                    missing.join(", ")
                )));
            }
        }
        Ok(())
    }

    pub fn typescript_declarations(&self) -> Result<String> {
        let catalog = InventoryCatalog::new();
        self.typescript_declarations_with_catalog(&catalog)
    }

    pub fn typescript_declarations_with_catalog<C: ToolCatalog>(
        &self,
        catalog: &C,
    ) -> Result<String> {
        let allowlist = {
            let inner = self.inner.lock().unwrap();
            inner.allowlist.clone()
        };
        let tools = if let Some(allowlist) = &allowlist {
            let mut tools = Vec::with_capacity(allowlist.len());
            let mut missing = Vec::new();
            for name in allowlist {
                match catalog.by_name(name) {
                    Some(metadata) => tools.push(metadata.clone()),
                    None => missing.push(name.to_string()),
                }
            }
            if !missing.is_empty() {
                return Err(BamlRtError::InvalidArgument(format!(
                    "Tool metadata missing for: {}",
                    missing.join(", ")
                )));
            }
            tools
        } else {
            self.export_metadata()
        };
        render_tool_typescript(&tools)
    }

    pub fn write_typescript_declarations(&self, path: &std::path::Path) -> Result<()> {
        let declarations = self.typescript_declarations()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(BamlRtError::Io)?;
        }
        std::fs::write(path, declarations).map_err(BamlRtError::Io)?;
        Ok(())
    }

    /// Open a tool session and return its session id.
    /// Open a tool session. `context_id` is the invocation scope id (always present).
    pub async fn open_session(
        &self,
        name: &str,
        open_input: Value,
        context_id: &ContextId,
    ) -> Result<ToolSessionId> {
        let start = std::time::Instant::now();
        let parsed = ToolName::parse(name)?;
        let session_id = ToolSessionId::random();
        let span = crate::spans::open_session(&session_id, &parsed);
        let _guard = span.enter();

        let (metadata, handler) = {
            let inner = self.inner.lock().unwrap();
            let (metadata, handler) = inner.tools.get(&parsed).ok_or_else(|| {
                BamlRtError::FunctionNotFound(format!("Tool '{}' not found", parsed))
            })?;
            if metadata.origin == ToolOrigin::Host
                && let Some(allowlist) = &inner.allowlist
                && !allowlist.contains(&parsed)
            {
                return Err(BamlRtError::InvalidArgument(format!(
                    "Tool '{}' is not declared in the manifest allowlist",
                    parsed
                )));
            }
            (metadata.clone(), handler.clone())
        };

        let ctx = ToolSessionContext {
            session_id: session_id.clone(),
            tool_name: metadata.name.clone(),
            context_id: context_id.clone(),
        };
        let session = handler.open_session(ctx, open_input).await?;
        {
            let mut inner = self.inner.lock().unwrap();
            inner
                .sessions
                .insert(session_id.clone(), Arc::new(TokioMutex::new(session)));
        }

        let duration = start.elapsed();
        crate::metrics::record_session_open(&parsed.to_string());
        crate::metrics::record_session_operation("open", duration);

        Ok(session_id)
    }

    pub async fn session_send(&self, session_id: &ToolSessionId, input: Value) -> Result<()> {
        let start = std::time::Instant::now();
        let span = crate::spans::session_send(session_id);
        let _guard = span.enter();

        let session = {
            let inner = self.inner.lock().unwrap();
            inner.sessions.get(session_id).cloned()
        };
        let session = session.ok_or_else(|| {
            if tool_registry_trace_enabled() {
                let inner = self.inner.lock().unwrap();
                tool_registry_trace(&format!(
                    "session_send missing: session_id={}, known_sessions={}",
                    session_id,
                    inner.sessions.len()
                ));
            }
            BamlRtError::InvalidArgument(format!("Unknown session {}", session_id))
        })?;
        let mut guard = session.lock().await;
        let result = guard.send(input).await.map_err(map_session_error);

        let duration = start.elapsed();
        crate::metrics::record_session_operation("send", duration);

        result
    }

    pub async fn session_next(&self, session_id: &ToolSessionId) -> Result<ToolStep> {
        let start = std::time::Instant::now();
        let span = crate::spans::session_next(session_id);
        let _guard = span.enter();

        let session = {
            let inner = self.inner.lock().unwrap();
            inner.sessions.get(session_id).cloned()
        };
        let session = session.ok_or_else(|| {
            if tool_registry_trace_enabled() {
                let inner = self.inner.lock().unwrap();
                tool_registry_trace(&format!(
                    "session_next missing: session_id={}, known_sessions={}",
                    session_id,
                    inner.sessions.len()
                ));
            }
            BamlRtError::InvalidArgument(format!("Unknown session {}", session_id))
        })?;
        let mut guard = session.lock().await;
        let result = guard.next().await.map_err(map_session_error);

        let duration = start.elapsed();
        crate::metrics::record_session_operation("next", duration);

        result
    }

    pub async fn session_finish(&self, session_id: &ToolSessionId) -> Result<()> {
        let start = std::time::Instant::now();
        let span = crate::spans::session_finish(session_id);
        let _guard = span.enter();

        let session = {
            let mut inner = self.inner.lock().unwrap();
            inner.sessions.remove(session_id)
        };
        if let Some(session) = session {
            let mut guard = session.lock().await;
            guard.finish().await.map_err(map_session_error)?;
        }

        let duration = start.elapsed();
        crate::metrics::record_session_operation("finish", duration);

        Ok(())
    }

    pub async fn session_abort(
        &self,
        session_id: &ToolSessionId,
        reason: Option<String>,
    ) -> Result<()> {
        let start = std::time::Instant::now();
        let span = crate::spans::session_abort(session_id, reason.as_deref());
        let _guard = span.enter();

        let session = {
            let mut inner = self.inner.lock().unwrap();
            inner.sessions.remove(session_id)
        };
        if let Some(session) = session {
            let mut guard = session.lock().await;
            guard.abort(reason).await.map_err(map_session_error)?;
        }

        let duration = start.elapsed();
        crate::metrics::record_session_operation("abort", duration);

        Ok(())
    }

    /// Execute a tool function by name (single-shot convenience). `context_id` is the invocation scope id.
    pub async fn execute(&self, name: &str, args: Value, context_id: &ContextId) -> Result<Value> {
        let start = std::time::Instant::now();
        let span = crate::spans::execute_tool(name);
        let _guard = span.enter();

        tracing::debug!(
            tool = name,
            args = ?args,
            "Executing tool function"
        );

        let parsed = ToolName::parse(name)?;
        let handler = {
            let inner = self.inner.lock().unwrap();
            let (_, handler) = inner.tools.get(&parsed).ok_or_else(|| {
                BamlRtError::FunctionNotFound(format!("Tool '{}' not found", parsed))
            })?;
            handler.clone()
        };
        if handler.capability() != ToolCapability::OneShot {
            return Err(BamlRtError::InvalidArgument(format!(
                "Tool '{}' requires a streaming session; use open_session",
                parsed
            )));
        }

        // For execute, open_input is always () (empty object)
        let session_id = self
            .open_session(&parsed.to_string(), empty_open_input(), context_id)
            .await?;
        self.session_send(&session_id, args).await?;
        let result = match self.session_next(&session_id).await? {
            ToolStep::Streaming { output } | ToolStep::Suspended { output } => {
                self.session_finish(&session_id).await?;
                Ok(output)
            }
            ToolStep::Done { output } => {
                self.session_finish(&session_id).await?;
                Ok(output.unwrap_or(Value::Null))
            }
            ToolStep::Error { error } => {
                self.session_abort(&session_id, Some(error.message.clone()))
                    .await?;
                Err(map_session_error(ToolSessionError::Tool(error)))
            }
        };

        // Record metrics
        let duration = start.elapsed();
        let result_str = if result.is_ok() { "success" } else { "error" };
        crate::metrics::record_tool_execution(name, result_str, duration);

        result
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Async handler: takes input I, returns a future that resolves to Result<O>.
type TypedToolHandler<I, O> =
    Arc<dyn Fn(I) -> Pin<Box<dyn Future<Output = Result<O>> + Send>> + Send + Sync>;

pub struct TypedToolFunction<I, O, F> {
    metadata: ToolFunctionMetadata,
    handler: TypedToolHandler<I, O>,
    _phantom: std::marker::PhantomData<(I, O, F)>,
}

impl<I, O, F> TypedToolFunction<I, O, F>
where
    I: ToolType + Serialize + for<'de> Deserialize<'de>,
    O: ToolType + Serialize,
    F: Fn(I) -> Pin<Box<dyn Future<Output = Result<O>> + Send>> + Send + Sync + 'static,
{
    pub fn new(name: &str, description: &str, handler: F) -> Self {
        // Tool name format is validated at compile time by the type system
        // If parsing fails, it indicates a programming error in the caller
        let (parsed, class_name) = parse_tool_name_and_class(name).unwrap_or_else(|e| {
            panic!(
                "Invalid tool name format '{}': {}. This is a programming error.",
                name, e
            )
        });
        let metadata = TypeBasedMetadataBuilder::<(), I, O>::new(
            parsed.clone(),
            class_name,
            description.to_string(),
        )
        .build_metadata();
        Self {
            metadata,
            handler: Arc::new(handler),
            _phantom: std::marker::PhantomData,
        }
    }
}

#[async_trait]
impl<I, O, F> ToolHandler for TypedToolFunction<I, O, F>
where
    I: ToolType + Serialize + for<'de> Deserialize<'de>,
    O: ToolType + Serialize,
    F: Fn(I) -> Pin<Box<dyn Future<Output = Result<O>> + Send>> + Send + Sync + 'static,
{
    fn metadata(&self) -> &ToolFunctionMetadata {
        &self.metadata
    }

    async fn open_session(
        &self,
        ctx: ToolSessionContext,
        open_input: Value,
    ) -> Result<Box<dyn ToolSession>> {
        // For TypedToolFunction, open_input is ignored (it's always ())
        // Validate it's an empty object or can be deserialized as unit type
        validate_open_input::<()>(open_input)?;

        let handler = self.handler.clone();
        let executor: Box<dyn ToolExecutor> = Box::new(ExecutorAdapter::new(move |input| {
            let parsed: I = match deserialize_tool_input(input) {
                Ok(value) => value,
                Err(err) => {
                    return Box::pin(async move { Err(err) });
                }
            };
            let future = handler(parsed);
            Box::pin(async move {
                let output = future.await?;
                serialize_tool_output(output)
            })
        }));
        Ok(Box::new(OneShotSession::new(ctx, executor)))
    }
}

/// Session tool with multiple Send/Next pairs, built from an async function and pre-built metadata.
///
/// Send and next are **paired** FSM steps: one send runs the executor, one next returns that
/// result (as `ToolStep::Done`). Whether and how the tool supports continuation (e.g. paging)
/// is up to the tool's input/output types; the session protocol is purely FSM advancement.
///
/// Open validates OI; each Send runs the executor and Next returns that result. Use when you have
/// runtime deps and want multi-request sessions without implementing [ToolHandler].
pub fn create_multi_send_session_tool_from_async<OI, I, O, F>(
    metadata: ToolFunctionMetadata,
    executor: F,
) -> Arc<dyn ToolHandler>
where
    OI: for<'de> Deserialize<'de> + Send + Sync + 'static,
    I: crate::tool_schema::ToolType + Serialize + for<'de> Deserialize<'de> + Send + Sync + 'static,
    O: crate::tool_schema::ToolType + Serialize + Send + Sync + 'static,
    F: Fn(I) -> Pin<Box<dyn Future<Output = Result<O>> + Send>> + Send + Sync + 'static,
{
    Arc::new(MultiSendSessionToolFromAsync::<OI, I, O, F> {
        metadata,
        executor: Arc::new(executor),
        _phantom: PhantomData,
    })
}

/// Internal handler for create_multi_send_session_tool_from_async.
struct MultiSendSessionToolFromAsync<OI, I, O, F> {
    metadata: ToolFunctionMetadata,
    executor: Arc<F>,
    _phantom: PhantomData<(OI, I, O)>,
}

#[async_trait]
impl<OI, I, O, F> ToolHandler for MultiSendSessionToolFromAsync<OI, I, O, F>
where
    OI: for<'de> Deserialize<'de> + Send + Sync + 'static,
    I: crate::tool_schema::ToolType + Serialize + for<'de> Deserialize<'de> + Send + Sync + 'static,
    O: crate::tool_schema::ToolType + Serialize + Send + Sync + 'static,
    F: Fn(I) -> Pin<Box<dyn Future<Output = Result<O>> + Send>> + Send + Sync + 'static,
{
    fn metadata(&self) -> &ToolFunctionMetadata {
        &self.metadata
    }

    async fn open_session(
        &self,
        ctx: ToolSessionContext,
        open_input: Value,
    ) -> Result<Box<dyn ToolSession>> {
        validate_open_input::<OI>(open_input)?;
        let handler = self.executor.clone();
        let executor: Box<dyn ToolExecutor> = Box::new(ExecutorAdapter::new(move |input| {
            let parsed: I = match deserialize_tool_input(input) {
                Ok(value) => value,
                Err(err) => return Box::pin(async move { Err(err) }),
            };
            let future = handler(parsed);
            Box::pin(async move {
                let output = future.await?;
                serialize_tool_output(output)
            })
        }));
        Ok(Box::new(MultiSendSession::new(ctx, executor)))
    }
}

struct OneShotSession {
    ctx: ToolSessionContext,
    executor: Box<dyn ToolExecutor>,
    input: Option<Value>,
    state: SessionPhase,
}

impl OneShotSession {
    fn new(ctx: ToolSessionContext, executor: Box<dyn ToolExecutor>) -> Self {
        Self {
            ctx,
            executor,
            input: None,
            state: SessionPhase::Open,
        }
    }
}

#[async_trait]
impl ToolSession for OneShotSession {
    async fn send(&mut self, input: Value) -> std::result::Result<(), ToolSessionError> {
        if self.input.is_some() {
            return Err(ToolSessionError::Tool(ToolFailure::invalid_input(
                "Tool session already has input",
            )));
        }
        self.input = Some(input);
        Ok(())
    }

    async fn next(&mut self) -> std::result::Result<ToolStep, ToolSessionError> {
        if self.state.is_closed() {
            return Ok(ToolStep::Done { output: None });
        }
        let input = self.input.take().ok_or_else(|| {
            ToolSessionError::Tool(ToolFailure::invalid_input(format!(
                "Tool session {} has no input",
                self.ctx.session_id
            )))
        })?;
        let output = match self.executor.execute(input).await {
            Ok(value) => value,
            Err(err) => {
                return Ok(ToolStep::Error {
                    error: ToolFailure::from_error(&err),
                });
            }
        };
        self.state.close();
        Ok(ToolStep::Done {
            output: Some(output),
        })
    }

    async fn finish(&mut self) -> std::result::Result<(), ToolSessionError> {
        self.state.close();
        Ok(())
    }

    async fn abort(
        &mut self,
        _reason: Option<String>,
    ) -> std::result::Result<(), ToolSessionError> {
        self.state.close();
        Ok(())
    }
}
