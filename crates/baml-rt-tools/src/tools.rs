// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Tool function registration system
//!
//! This module provides a trait-based system for registering tool functions
//! that can be called by LLMs during BAML function execution or directly from JavaScript.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    future::Future,
    marker::PhantomData,
    pin::Pin,
    sync::{Arc, Mutex as StdMutex},
};

use async_trait::async_trait;
use baml_derive::BamlType;
use baml_rt_core::{
    BamlFunctionId, BamlPromptName, BamlRtError, ContextId, EventSourceKind, Result,
    SessionLifecycleError, VariantPhase,
    ids::{AgentId, TaskId},
};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex as TokioMutex;
use tracing::Instrument;

use crate::{
    bundles::BundleType,
    config_resolver::ConfigResolver,
    opaque_json::OpaqueJson,
    tool_catalog::{InventoryCatalog, ToolCatalog},
    tool_error_classify::{ClassifiedToolError, ToolExecutionClassifier},
    tool_fsm::{
        SessionPhase, ToolFailure, ToolSession, ToolSessionError, ToolSessionId, ToolStep,
        tool_failure_to_baml_tool_execution_error,
    },
    tool_schema::{ActionIdentity, DescribeAction, ToolType, json_schema_value},
    ts_gen::render_tool_typescript,
};

/// Access level a tool declares it needs. Serialized as lowercase
/// (`"read"` / `"write"` / `"delete"`) so `tool-manifest.json` stays
/// human-readable and matches the canonical spelling used by the runtime,
/// builder, and CLI scaffolder.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ToolAccess {
    Read,
    Write,
    Delete,
}

impl ToolAccess {
    /// Every declared access class. The single source of truth for "what
    /// classes exist"; iterate this anywhere you need to enumerate them
    /// instead of hard-coding the variant list.
    pub const ALL: &'static [ToolAccess] =
        &[ToolAccess::Read, ToolAccess::Write, ToolAccess::Delete];

    /// Canonical lowercase spelling used in `tool-manifest.json` and
    /// everywhere the access level is serialized as a string.
    pub fn as_str(self) -> &'static str {
        match self {
            ToolAccess::Read => "read",
            ToolAccess::Write => "write",
            ToolAccess::Delete => "delete",
        }
    }
}

impl std::fmt::Display for ToolAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Capitalize the first character of a string.
///
/// Used for lightweight identifier shaping where separator-aware handling is not needed.
pub(crate) fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// Convert identifier components to PascalCase for generated type/class names.
///
/// Splits on `-` / `_`, capitalizes each segment's first character, and joins.
/// This keeps runtime tool IDs unchanged while guaranteeing codegen-safe
/// class/type identifiers for BAML/TS.
fn to_pascal_identifier_component(s: &str) -> String {
    s.split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(capitalize_first)
        .collect()
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
/// use baml_rt_tools::{BamlTool, DescribeAction, Support};
/// use baml_rt_core::Result;
/// use serde::{Deserialize, Serialize};
/// use baml_derive::BamlType;
/// use async_trait::async_trait;
///
/// struct WeatherTool;
///
/// #[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
/// struct WeatherInput {
///     location: String,
/// }
///
/// #[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
/// struct WeatherOutput {
///     temperature: String,
///     location: String,
/// }
///
/// impl DescribeAction for WeatherInput {
///     fn describe(&self) -> String {
///         format!("weather for {}", self.location)
///     }
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

    /// Session policy: Strict (one Send per session) or MultiSend (multiple Sends).
    /// Override to MultiSend for tools where a typical workflow requires multiple
    /// different queries in one session (e.g. search pages then read blocks).
    const SESSION_POLICY: SessionPolicy = SessionPolicy::Strict;

    /// Typed input for opening the session (initial_input in Open step).
    /// Use `()` for tools that don't need args when opening.
    type OpenInput: ToolType + Serialize + for<'de> Deserialize<'de> + DescribeAction;

    /// Typed input for sending to an open session (input in Send step).
    /// Must implement `DescribeAction` to produce natural language prose
    /// for drift scoring and context summarisation.
    type Input: ToolType + Serialize + for<'de> Deserialize<'de> + DescribeAction;

    /// Typed output for this tool.
    type Output: ToolType + Serialize + for<'de> Deserialize<'de>;

    /// The unique qualified name of this tool (derived from Bundle::NAME and LOCAL_NAME)
    fn name() -> String {
        format!("{}/{}", Self::Bundle::NAME, Self::LOCAL_NAME)
    }

    /// The class name for BAML generation (e.g., "SupportCalculate" from Support + Calculate).
    ///
    /// Runtime tool IDs can contain `-` / `_`; generated class names are always
    /// PascalCase-safe identifiers.
    fn class_name() -> String {
        let bundle_name = Self::Bundle::NAME;
        let local_name = Self::LOCAL_NAME;
        format!(
            "{}{}",
            to_pascal_identifier_component(bundle_name),
            to_pascal_identifier_component(local_name)
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

    /// Compact a tool result and produce a natural language summary.
    ///
    /// Mutates the output for token-efficient context projection (truncation,
    /// One-line natural language description of what the tool result contains.
    /// Used as the archive header summary and as the `tool_result` history item.
    ///
    /// Default returns a generic fallback. Tools override with a specific
    /// description like `"found 3 records matching 'bob'"` or
    /// `"message sent, waiting for response"`.
    fn describe_result(&self, _output: &Self::Output) -> String {
        format!("{} result", Self::LOCAL_NAME)
    }

    /// Describe what engaging this tool means, returned for Open FSM steps.
    /// Used for intent drift detection at session boundaries.
    fn describe_open(&self) -> String {
        format!("using {}", Self::LOCAL_NAME)
    }

    /// Describe the typed open-input for a richer Open step description.
    /// Default ignores the input and delegates to `describe_open`.
    fn describe_open_input(&self, _input: &Self::OpenInput) -> String {
        self.describe_open()
    }

    /// Describe a specific tool action as natural language prose (present
    /// participle), for Send FSM steps.
    /// Default delegates to `DescribeAction::describe` on the Input type.
    fn describe_invocation(&self, input: &Self::Input) -> String {
        input.describe()
    }

    /// Structured, compact action/input identity for this tool's Send input.
    ///
    /// Return `None` for flat/single-action tools where schema-driven projection is sufficient.
    /// Multi-action untagged-union tools should return an [`ActionIdentity`] so the runtime can
    /// preserve semantic operation names while still owning formatting, redaction, and truncation.
    fn action_identity(&self, _input: &Self::Input) -> Option<ActionIdentity> {
        None
    }

    /// Classify execution failures for host retry policy and LLM-visible `tool_error` payloads.
    fn classify_execution_error(err: &BamlRtError) -> ClassifiedToolError {
        ClassifiedToolError::from_baml_error(err)
    }
}

/// Type of secret required by a tool (determines provisioning UX and validation).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretType {
    /// Static API key (e.g. pk_..., sk_...).
    ApiKey,
    /// OAuth 2.0 access token (bearer).
    OAuthAccessToken,
    /// OAuth 2.0 refresh token.
    OAuthRefreshToken,
    /// Username + password pair.
    BasicAuth,
    /// TLS certificate or private key.
    Certificate,
    /// Custom / vendor-specific.
    Other(String),
}

impl SecretType {
    pub fn other(value: impl Into<String>) -> Self {
        Self::Other(value.into())
    }

    /// Snake_case string for API/JSON (e.g. "api_key", "oauth_access_token").
    pub fn as_str(&self) -> &str {
        match self {
            Self::ApiKey => "api_key",
            Self::OAuthAccessToken => "oauth_access_token",
            Self::OAuthRefreshToken => "oauth_refresh_token",
            Self::BasicAuth => "basic_auth",
            Self::Certificate => "certificate",
            Self::Other(s) => s.as_str(),
        }
    }
}

/// Declares a secret required by a tool to access a remote service.
///
/// The `descriptor` field describes what the secret must provide — access level,
/// OAuth scopes, permissions, format hints — so operators know exactly what to provision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretRequest {
    /// Canonical name for the secret (e.g. env var name, config field).
    pub name: String,

    /// Type of secret (determines provisioning UX and validation).
    pub secret_type: SecretType,

    /// Human-readable justification: why this tool needs this secret.
    pub justification: String,

    /// Descriptor of what the secret must provide: access level, OAuth scopes,
    /// service-specific permissions, etc.
    pub descriptor: String,
}

impl SecretRequest {
    pub fn api_key(
        name: impl Into<String>,
        justification: impl Into<String>,
        descriptor: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            secret_type: SecretType::ApiKey,
            justification: justification.into(),
            descriptor: descriptor.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolTypeSpec {
    pub name: String,
    pub ts_decl: Option<String>,
}

/// Per-tool config metadata; absent means tool has no config.
/// Every config bundle **must** specify a default (required field); use
/// [`ToolConfigMetadata::default_from_schema`] when building from a schema that has
/// `properties.<key>.default`, or pass an explicit value (e.g. `json!({})`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConfigMetadata {
    /// JSON Schema for the config.
    pub schema: Value,
    /// Default config. Required: metadata must specify a default (strict typing).
    pub default: Value,
    /// Type name for TS/BAML generation.
    pub type_name: Option<String>,
}

impl ToolConfigMetadata {
    /// Build metadata with required schema and default. Use this to enforce that every
    /// bundle specifies a default at construction time.
    pub fn new(schema: Value, default: Value, type_name: Option<String>) -> Self {
        Self {
            schema,
            default,
            type_name,
        }
    }

    /// Derive a default config object from JSON Schema when `properties` entries have a `default` key.
    /// Returns `None` if the schema is not an object with `properties` or no property has a default.
    /// Use when building [`ToolConfigMetadata`]: pass this or an explicit value so the bundle always has a default.
    pub fn default_from_schema(schema: &Value) -> Option<Value> {
        let obj = schema.as_object()?;
        let props = obj.get("properties")?.as_object()?;
        let mut out = serde_json::Map::new();
        for (k, v) in props {
            let prop_schema = v.as_object()?;
            if let Some(default) = prop_schema.get("default") {
                out.insert(k.clone(), default.clone());
            }
        }
        if out.is_empty() {
            return None;
        }
        Some(Value::Object(out))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolProjectionSemantics {
    pub identity: String,
    pub summary: String,
    pub detail: String,
}

fn is_valid_bundle_identifier_component(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

fn is_valid_local_tool_identifier_component(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BundleName(String);

impl BundleName {
    pub fn new(name: impl Into<String>) -> Result<Self> {
        let name = name.into();
        if !is_valid_bundle_identifier_component(&name) {
            return Err(BamlRtError::InvalidArgument(format!(
                "Bundle name '{}' must match ^[a-z][a-z0-9_-]*$ (ASCII lowercase letters, digits, '_' or '-')",
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
        if !is_valid_local_tool_identifier_component(&name) {
            return Err(BamlRtError::InvalidArgument(format!(
                "Tool name '{}' must match ^[a-z][A-Za-z0-9_-]*$ (ASCII letters, digits, '_' or '-', starting with lowercase)",
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
    /// MCP-imported tools use a three-part presentation form
    /// `mcp/<server>/<tool>`. Internally we fold this into the standard
    /// two-part shape with `bundle = mcp_<server>` and `local = <tool>` so
    /// the rest of the registry, catalog, and codegen stack treats the
    /// imported tool like any other host tool. `Display` reverses the fold
    /// so manifest references and tracing output keep the readable form.
    pub fn parse(name: &str) -> Result<Self> {
        let parts: Vec<&str> = name.split('/').collect();
        if parts.len() == 3 && parts[0] == "mcp" {
            return Ok(Self {
                bundle: BundleName::new(format!("mcp_{}", parts[1]))?,
                local: LocalToolName::new(parts[2].to_string())?,
            });
        }
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

    /// Derived slug for codegen identifiers: `"support_calculate"` from `"support/calculate"`.
    ///
    /// Normalizes `-` to `_` in bundle/local components so generated BAML
    /// function names remain valid identifiers.
    pub fn slug(&self) -> ToolSlug {
        fn normalize_slug_component(s: &str) -> String {
            s.replace('-', "_")
        }

        ToolSlug(format!(
            "{}_{}",
            normalize_slug_component(self.bundle.as_str()),
            normalize_slug_component(self.local.as_str())
        ))
    }
}

impl std::fmt::Display for ToolName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(server) = self.bundle.as_str().strip_prefix("mcp_") {
            write!(f, "mcp/{}/{}", server, self.local)
        } else {
            write!(f, "{}/{}", self.bundle, self.local)
        }
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

/// Codegen-safe identifier derived from a `ToolName`: `"support/calculate"` -> `"support_calculate"`.
///
/// Used in generated BAML function names (`__active__support_calculate`) and as keys in
/// phase function mappings. Constructed only via `ToolName::slug()`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolSlug(String);

impl ToolSlug {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ToolSlug {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Newtype for a session plan type name (e.g. `"SupportCalculateSessionPlan"`).
///
/// Wraps the BAML class name that the builder generates for each tool's session plan.
/// Invariant: the inner string must end with `"SessionPlan"`.
/// `class_name()` strips the suffix — infallible by construction.
/// Lives in `baml-rt-tools` so both the builder and runtime crates can use the same type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionPlanTypeName(String);

impl SessionPlanTypeName {
    /// Construct a validated `SessionPlanTypeName`.
    /// Rejects values that do not end with `"SessionPlan"`.
    pub fn new(name: impl Into<String>) -> Result<Self> {
        let name = name.into();
        if !name.ends_with("SessionPlan") {
            return Err(BamlRtError::InvalidArgument(format!(
                "SessionPlanTypeName '{name}' must end with 'SessionPlan'"
            )));
        }
        Ok(Self(name))
    }

    /// Derive the tool class name: `"SupportCalculateSessionPlan"` → `"SupportCalculate"`.
    /// Infallible — the suffix invariant is guaranteed by construction.
    pub fn class_name(&self) -> &str {
        self.0.strip_suffix("SessionPlan").unwrap()
    }

    /// Borrow the inner type name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SessionPlanTypeName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for SessionPlanTypeName {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SessionPlanTypeName {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let name = String::deserialize(deserializer)?;
        SessionPlanTypeName::new(name).map_err(serde::de::Error::custom)
    }
}

/// Map from BAML function name to candidate session plan types.
///
/// Length 1 = single-tool (existing behavior).
/// Length >1 = polymorphic Open — the LLM selects a tool via `tool_name` on the Open step.
pub type SessionPlanFunctionsMap = std::collections::HashMap<String, Vec<SessionPlanTypeName>>;

/// Authoring/build manifest: base BAML function names that use the unified structured step
/// executor (`__entry` only, `run_step_executor_loop` unified mode — plan/synthesis/archive/AskUser).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UnifiedStepExecutorRootConfig {
    #[serde(default = "default_include_archive_reads")]
    pub include_archive_reads: bool,
}

fn default_include_archive_reads() -> bool {
    true
}

pub type UnifiedStepExecutorFunctionsMap =
    std::collections::HashMap<String, UnifiedStepExecutorRootConfig>;

/// Parses authoring `baml_src/unified_step_executors.json`: either `{ "roots": { … } }` or a flat
/// map from base function name → [`UnifiedStepExecutorRootConfig`].
pub fn parse_unified_step_executors_authoring_json(text: &str) -> UnifiedStepExecutorFunctionsMap {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return UnifiedStepExecutorFunctionsMap::new();
    };
    if let Some(obj) = v.get("roots").and_then(|r| r.as_object()) {
        serde_json::from_value(serde_json::Value::Object(obj.clone())).unwrap_or_default()
    } else {
        serde_json::from_value(v).unwrap_or_default()
    }
}

/// The role a BAML function plays in the step executor pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FunctionRole {
    /// User-authored root step executor (e.g. `ExecuteStep`).
    Root,
    /// Generated entry phase (`__entry`): archive reuse, read-only finish, or Open.
    Entry,
    /// Generated active session phase (`__active__<slug>`): Send, reads, Finish, Abort after Open.
    Active,
    /// Reserved: `__consume__<slug>` phase (manifest only if such functions exist in IR; builder does not emit them yet).
    Consume,
}

/// Enriched binding for a function in the session plan manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionPlanBinding {
    pub plan_types: Vec<SessionPlanTypeName>,
    pub role: FunctionRole,
}

/// Canonical naming conventions for generated session types and phase functions.
///
/// Phase function names are built via `BamlFunctionId::variant` + `full_name` in `baml-rt-core`
/// so `BamlFunctionId::parse` and codegen stay mutually consistent.
pub struct SessionTypeNames;

impl SessionTypeNames {
    pub fn entry(base: &str) -> String {
        BamlFunctionId::variant(BamlPromptName::new(base), VariantPhase::Entry).full_name()
    }

    pub fn active(base: &str, slug: &ToolSlug) -> String {
        BamlFunctionId::variant(
            BamlPromptName::new(base),
            VariantPhase::Active {
                tool_slug: slug.as_str().to_string(),
            },
        )
        .full_name()
    }

    pub fn consume(base: &str, slug: &ToolSlug) -> String {
        BamlFunctionId::variant(
            BamlPromptName::new(base),
            VariantPhase::Consume {
                tool_slug: slug.as_str().to_string(),
            },
        )
        .full_name()
    }

    pub fn open_step(class_name: &str) -> String {
        format!("{class_name}OpenStep")
    }

    pub fn session_plan(class_name: &str) -> String {
        format!("{class_name}SessionPlan")
    }
}

/// How the step executor schedules Send vs archive read ops for a tool session.
///
/// `Strict` (the default) enforces a single Send per hop: after a Send the
/// executor may only SearchRead or PageRead, preventing "Tool session already has input" errors
/// that occur when the LLM picks Send again on a session with pending output.
///
/// `MultiSend` is an opt-in for tools that genuinely support sending multiple
/// payloads within one open session before reading results.
///
/// ## SessionPolicy Invariants
///
/// 1. **Policy resolution from metadata**:
///    ∀ step executor calls: policy is resolved from `ToolFunctionMetadata.session_policy`
///    via the `session_plan_functions` manifest, never from function name matching.
///
/// 2. **Strict mode safety**:
///    After Send, the host FSM only advances with SearchRead, PageRead, Finish, or
///    (in `MultiSend`) another Send — preventing "Tool session already has input" errors.
///
/// 3. **Default safety**:
///    Unknown tools default to `Strict` (prevents double-send bugs).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SessionPolicy {
    /// Open → Send → SearchRead | PageRead → Finish, one Send per hop (default).
    #[default]
    Strict,
    /// Open → Send* → SearchRead | PageRead → Finish, multiple Sends allowed per session.
    MultiSend,
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
    /// Declared secrets required by this tool (name, type, justification, descriptor).
    pub secret_requests: Vec<SecretRequest>,
    /// Config schema and defaults; absent when tool has no config.
    pub config: Option<ToolConfigMetadata>,
    /// Bundle key for config store lookup. When set, config is resolved by this bundle name
    /// (tools in the same config scope share config). Must be set when `config` is `Some`.
    pub config_bundle: Option<BundleName>,
    /// Origin of this tool (host vs guest)
    pub origin: ToolOrigin,
    /// Execution backend (Static, External, MCP, Sandbox, or Wasm). Defaults to `Static`.
    pub backend: ToolBackend,
    /// Content-addressed digest for external tool artifact / package bytes.
    /// `None` for static tools and for external tools when verification is disabled.
    pub digest: Option<String>,
    /// Optional tool-specific semantics for projection modes used during SearchRead/PageRead steps.
    pub projection_semantics: Option<ToolProjectionSemantics>,
    /// FSM scheduling policy for the step executor. Controls which ops are
    /// offered after a Send. Defaults to `Strict` (one Send per hop).
    pub session_policy: SessionPolicy,
    /// Event source kinds this tool can produce when polled.
    /// Empty means the tool is invoke-only (no event production).
    pub event_sources: Vec<EventSourceKind>,
    /// Optional session-coordination BAML fragment shipped with the tool.
    ///
    /// Authored once per tool (by the tool author) and merged into the builder's
    /// generated prelude when an agent lists the tool in its manifest. Internal
    /// tools populate this via `inventory!` providers in `session_coordination.rs`;
    /// external tools populate it from a `coordination.baml_file` declared in
    /// `tool-manifest.json`. `None` for tools that do not need coordination
    /// (single-shot invokes, internal tools without a `Choose<X>Action` flow).
    pub coordination_baml: Option<String>,
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

    /// Derive class name from bundle and local tool names.
    ///
    /// Runtime IDs remain unchanged (`bundle/local`), but derived class names
    /// are normalized to PascalCase for BAML-safe identifiers.
    pub fn derive_class_name(bundle: &BundleName, local: &LocalToolName) -> String {
        format!(
            "{}{}",
            to_pascal_identifier_component(bundle.as_str()),
            to_pascal_identifier_component(local.as_str())
        )
    }

    /// Create ToolFunctionMetadata from type parameters
    ///
    /// This helper consolidates the common pattern of building metadata
    /// from type information, reducing duplication across registration sites.
    #[expect(
        clippy::too_many_arguments,
        reason = "constructor derives metadata from type parameters; the inputs have no natural grouping"
    )]
    pub fn from_types<OpenInput, Input, Output>(
        name: ToolName,
        class_name: String,
        description: String,
        tags: Vec<String>,
        secret_requests: Vec<SecretRequest>,
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
            secret_requests,
            config: None,
            config_bundle: None,
            origin,
            backend: ToolBackend::default(),
            digest: None,
            projection_semantics: None,
            session_policy: SessionPolicy::default(),
            event_sources: Vec::new(),
            coordination_baml: None,
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
    secret_requests: Vec<SecretRequest>,
    config: Option<ToolConfigMetadata>,
    config_bundle: Option<BundleName>,
    origin: ToolOrigin,
    backend: ToolBackend,
    digest: Option<String>,
    projection_semantics: Option<ToolProjectionSemantics>,
    extra_ts_decls: Vec<String>,
    access: Option<ToolAccess>,
    session_policy: SessionPolicy,
    event_sources: Vec<EventSourceKind>,
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
            secret_requests: Vec::new(),
            config: None,
            config_bundle: None,
            origin: ToolOrigin::Host,
            backend: ToolBackend::default(),
            digest: None,
            projection_semantics: None,
            session_policy: SessionPolicy::default(),
            event_sources: Vec::new(),
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

    /// Set declared secret requests for the tool.
    pub fn with_secret_requests(mut self, requests: Vec<SecretRequest>) -> Self {
        self.secret_requests = requests;
        self
    }

    /// Set config schema and defaults for the tool.
    pub fn with_config(mut self, config: ToolConfigMetadata) -> Self {
        self.config = Some(config);
        self
    }

    /// Set the bundle key for config store lookup. Required when config is set.
    pub fn with_config_bundle(mut self, bundle_name: BundleName) -> Self {
        self.config_bundle = Some(bundle_name);
        self
    }

    /// Set the tool origin
    pub fn with_origin(mut self, origin: ToolOrigin) -> Self {
        self.origin = origin;
        self
    }

    /// Set explicit digest for external tool artifact/package bytes.
    pub fn with_digest(mut self, digest: String) -> Self {
        self.digest = Some(digest);
        self
    }

    /// Set explicit semantics for identity/summary/detail read projections.
    pub fn with_projection_semantics(
        mut self,
        identity: impl Into<String>,
        summary: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        self.projection_semantics = Some(ToolProjectionSemantics {
            identity: identity.into(),
            summary: summary.into(),
            detail: detail.into(),
        });
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

    /// Override the FSM scheduling policy for this tool.
    pub fn with_session_policy(mut self, policy: SessionPolicy) -> Self {
        self.session_policy = policy;
        self
    }

    /// Set event source kinds this tool can produce when polled.
    pub fn with_event_sources(mut self, event_sources: Vec<EventSourceKind>) -> Self {
        self.event_sources = event_sources;
        self
    }

    /// Set the execution backend for this tool.
    pub fn with_backend(mut self, backend: ToolBackend) -> Self {
        self.backend = backend;
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
            self.secret_requests,
            self.origin,
            self.extra_ts_decls,
            self.access,
        );
        metadata.baml_decl = self.baml_decl;
        metadata.config = self.config;
        metadata.config_bundle = self.config_bundle;
        metadata.backend = self.backend;
        metadata.digest = self.digest;
        metadata.projection_semantics = self.projection_semantics;
        metadata.session_policy = self.session_policy;
        metadata.event_sources = self.event_sources;
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
    pub secret_requests: Vec<SecretRequest>,
    pub config: Option<ToolConfigMetadata>,
    pub origin: ToolOrigin,
    #[serde(default)]
    pub backend: ToolBackend,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    pub projection_semantics: Option<ToolProjectionSemantics>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub event_sources: Vec<EventSourceKind>,
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
            secret_requests: metadata.secret_requests.clone(),
            config: metadata.config.clone(),
            origin: metadata.origin,
            backend: metadata.backend,
            digest: metadata.digest.clone(),
            projection_semantics: metadata.projection_semantics.clone(),
            event_sources: metadata.event_sources.clone(),
        }
    }
}

/// Discovery record for tool search (name, bundle, description, tags, access, origin, event_sources).
/// Lists globally available tools only; no per-agent invokability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDiscoveryRecord {
    pub name: ToolName,
    pub bundle: BundleName,
    pub description: String,
    pub tags: Vec<String>,
    pub access: Option<ToolAccess>,
    pub origin: ToolOrigin,
    #[serde(default)]
    pub backend: ToolBackend,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub event_sources: Vec<EventSourceKind>,
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
            backend: metadata.backend,
            digest: metadata.digest.clone(),
            event_sources: metadata.event_sources.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolBundleMetadata {
    pub name: BundleName,
    pub description: String,
    pub config_schema: Option<Value>,
    pub secret_requests: Vec<SecretRequest>,
}

/// Declares whether this tool ever emits `ToolStep::Streaming` from `read()`.
///
/// The session protocol: after `send(input)`, the caller calls `read(input)` until the step
/// indicates completion. Each `read(input)` returns a `ToolStep`:
/// `Streaming { output }` (more to come),
/// `Done { output }` (completion), or `Error { error }`.
///
/// - **OneShot:** This tool only ever returns `Done` or `Error` from `read(input)`. One read after
///   a `send()` returns the full result and signals completion. `ToolRegistry::execute()` (one
///   open → send → read → finish) is allowed.
/// - **Streaming:** This tool may return `ToolStep::Streaming` one or more times. Each read may
///   block or buffer and does *not* indicate completion until the step is `Done`. Callers must
///   use `open_session` and call `read(input)` in a loop until `Done`. `execute()` is disabled.
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

/// Execution backend for a tool. Orthogonal to [`ToolOrigin`] (ownership).
///
/// - `Static`: compiled into the runner at build time and linked via inventory.
/// - `External`: resolved at deploy time from a local external-tool artifact/package.
/// - `Mcp`: resolved from an approved MCP snapshot and invoked via MCP transport.
/// - `Sandbox`: runs inside a microsandbox-backed microVM; dispatch lands in Workstream B.
/// - `Wasm`: loaded per invocation into a Wasm sandbox (future).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolBackend {
    /// Compiled into the runner via inventory (current default).
    #[default]
    Static,
    /// Runs as a standalone process speaking the platform external-tool protocol over stdio/UDS.
    External,
    /// Runs through the Model Context Protocol using an approved snapshot/cache entry.
    Mcp,
    /// Runs inside a microsandbox-backed microVM (Workstream B).
    Sandbox,
    /// Runs inside a Wasm sandbox (future).
    Wasm,
}

/// Context passed to a tool when a session is opened. context_id and agent_id (from invocation
/// scope) are set by the executor so tools (e.g. internal_a2a) can attach context_id to
/// outbound requests for session continuity; agent_id is used for workspace resolution etc.
#[derive(Clone)]
pub struct ToolSessionContext {
    pub session_id: ToolSessionId,
    pub tool_name: ToolName,
    /// Invocation scope context_id; used by internal_a2a for delegated session continuity.
    pub context_id: ContextId,
    /// Invocation scope agent_id; used for workspace resolution and attribution.
    pub agent_id: AgentId,
    /// Resolved config at session open; None if tool has no config schema or no config stored.
    pub config: Option<Value>,
    /// Config version when config was resolved; used for provenance linkage.
    pub config_version: Option<u64>,
    /// Invocation scope task_id when available.
    pub task_id: Option<TaskId>,
    /// Optional per-tool classifier for execution errors (set when opening via [`ToolWrapper`]).
    pub execution_classifier: Option<ToolExecutionClassifier>,
}

/// Generic explicit read request envelope for Send steps.
///
/// This is runtime-general and tool-agnostic. Tools may opt into supporting one or more
/// read modes and must validate mode-specific constraints.
#[derive(Debug, Clone, Serialize, Deserialize, BamlType, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionReadMode {
    #[serde(alias = "RETRIEVE_REF", alias = "RetrieveRef")]
    RetrieveRef,
}

/// Host-reported archive read hop in [`HistoryContextV1`] (matches session FSM read ops).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "PascalCase")]
pub enum HistoryContextSessionOp {
    SearchRead,
    PageRead,
}

/// Status of a host-reported session read hop in [`HistoryContextV1`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "lowercase")]
pub enum HistoryContextStatus {
    Done,
    Streaming,
    Suspended,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
#[serde(rename_all = "camelCase")]
pub struct SessionReadEnvelope {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<SessionReadMode>,
    pub ref_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projection: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_hint: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryContextV1 {
    pub hop: u32,
    pub op: HistoryContextSessionOp,
    pub status: HistoryContextStatus,
    #[serde(default)]
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    // History payloads are opaque host-side state carried across read hops.
    // The explicit `OpaqueJson` wrapper keeps generated interfaces honest while
    // preserving arbitrary JSON at runtime.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<BTreeMap<String, OpaqueJson>>,
}

const ARCHIVE_ACTION_MAX_VALUE_CHARS: usize = 32;
const ARCHIVE_ACTION_MAX_TOTAL_CHARS: usize = 96;
pub const ARCHIVE_ACTION_MAX_IDENTITY_FIELDS: usize = 3;

/// Conservative field-name check for redaction in archive headers.
///
/// Substring/suffix rules (`*_key`, `*_token`, `*secret*`) over-match deliberately:
/// the cost of redacting a benign `cache_key` in a debug header is trivial; the cost
/// of leaking a real secret to LLM context is not. Tighten only with an allowlist.
fn is_sensitive_archive_field(name: &str) -> bool {
    let bytes = name.as_bytes();
    let eq = |s: &str| bytes.eq_ignore_ascii_case(s.as_bytes());
    let ends_with_ci = |suffix: &str| {
        bytes.len() >= suffix.len()
            && bytes[bytes.len() - suffix.len()..].eq_ignore_ascii_case(suffix.as_bytes())
    };
    let contains_ci = |needle: &str| {
        let n = needle.as_bytes();
        bytes.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n))
    };
    eq("authorization")
        || eq("password")
        || eq("secret")
        || eq("api_key")
        || eq("apikey")
        || eq("token")
        || ends_with_ci("_key")
        || ends_with_ci("_token")
        || contains_ci("secret")
}

fn truncate_archive_string(s: &str, max: usize) -> String {
    let mut out = String::new();
    for (idx, ch) in s.chars().enumerate() {
        if idx >= max {
            out.push('…');
            return out;
        }
        out.push(ch);
    }
    out
}

fn archive_value_to_token(field: &str, value: &Value) -> Option<String> {
    if value.is_null() {
        return None;
    }
    if is_sensitive_archive_field(field) {
        return Some(format!("{field}=<redacted>"));
    }
    match value {
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => Some(format!(
            "{field}={:?}",
            truncate_archive_string(s, ARCHIVE_ACTION_MAX_VALUE_CHARS)
        )),
        Value::Number(n) => Some(format!("{field}={n}")),
        Value::Bool(b) => Some(format!("{field}={b}")),
        Value::Array(a) => Some(format!("{field}=[{} items]", a.len())),
        Value::Object(_) => Some(format!("{field}={{...}}")),
        Value::Null => None,
    }
}

fn schema_default_for<'a>(schema: Option<&'a Value>, field: &str) -> Option<&'a Value> {
    schema?.get("properties")?.get(field)?.get("default")
}

fn push_archive_field_token(
    tokens: &mut Vec<String>,
    input_obj: &serde_json::Map<String, Value>,
    schema: Option<&Value>,
    field: &str,
) {
    let Some(value) = input_obj.get(field) else {
        return;
    };
    if schema_default_for(schema, field).is_some_and(|default| default == value) {
        return;
    }
    if let Some(token) = archive_value_to_token(field, value) {
        tokens.push(token);
    }
}

fn schema_property_order(schema: Option<&Value>) -> Vec<String> {
    schema
        .and_then(|s| s.get("properties"))
        .and_then(Value::as_object)
        .map(|props| props.keys().cloned().collect())
        .unwrap_or_default()
}

fn tagged_one_of_archive_tokens(
    input_obj: &serde_json::Map<String, Value>,
    schema: Option<&Value>,
) -> Option<Vec<String>> {
    let branches = schema?.get("oneOf")?.as_array()?;
    for branch in branches {
        let Some(props) = branch.get("properties").and_then(Value::as_object) else {
            continue;
        };
        let Some((disc_field, action_value)) = props.iter().find_map(|(name, spec)| {
            let c = spec.get("const")?.as_str()?;
            if input_obj.get(name).and_then(Value::as_str) == Some(c) {
                Some((name.as_str(), c.to_string()))
            } else {
                None
            }
        }) else {
            continue;
        };
        let mut tokens = vec![action_value];
        if let Some(required) = branch.get("required").and_then(Value::as_array) {
            for field in required.iter().filter_map(Value::as_str) {
                if field != disc_field {
                    push_archive_field_token(&mut tokens, input_obj, Some(branch), field);
                }
            }
        }
        for field in schema_property_order(Some(branch)) {
            if field != disc_field && !tokens.iter().any(|t| t.starts_with(&format!("{field}="))) {
                push_archive_field_token(&mut tokens, input_obj, Some(branch), &field);
            }
        }
        return Some(tokens);
    }
    None
}

/// Render a compact action/input identity from typed, implementor-selected action parts.
///
/// The renderer keeps formatting, redaction, value truncation, field-count limits, and total-length
/// limits centralized. At most [`ARCHIVE_ACTION_MAX_IDENTITY_FIELDS`] fields are rendered, in the
/// order provided; if additional non-empty fields were supplied, an ellipsis token is appended so
/// the model knows the identity was compacted.
#[must_use]
pub fn project_archive_action_identity_from_parts<'a, I>(
    op: &str,
    action_name: Option<&str>,
    fields: I,
) -> String
where
    I: IntoIterator<Item = (&'a str, Value)>,
{
    let mut tokens = Vec::new();
    if let Some(name) = action_name.filter(|name| !name.is_empty()) {
        tokens.push(name.to_string());
    }

    let mut rendered_fields = 0;
    let mut omitted_fields = false;
    for (field, value) in fields {
        let Some(token) = archive_value_to_token(field, &value) else {
            continue;
        };
        if rendered_fields >= ARCHIVE_ACTION_MAX_IDENTITY_FIELDS {
            omitted_fields = true;
            break;
        }
        tokens.push(token);
        rendered_fields += 1;
    }
    if omitted_fields {
        tokens.push("…".to_string());
    }

    if tokens.is_empty() {
        return op.to_string();
    }
    let op_chars = op.chars().count();
    let budget = ARCHIVE_ACTION_MAX_TOTAL_CHARS.saturating_sub(op_chars + 2);
    let inner = tokens.join(" ");
    if inner.chars().count() <= budget {
        format!("{op}({inner})")
    } else {
        let trimmed = truncate_archive_string(&inner, budget.saturating_sub(1));
        format!("{op}({trimmed})")
    }
}

/// Render a compact action/input identity for archive headers, e.g.
/// `Send(location_query="Paris, France")` or `Send(list_tasks list_id="abc")`.
#[must_use]
pub fn project_archive_action_identity(op: &str, input: &Value, schema: Option<&Value>) -> String {
    let mut tokens = Vec::new();
    if let Some(input_obj) = input.as_object() {
        if let Some(tagged) = tagged_one_of_archive_tokens(input_obj, schema) {
            tokens = tagged;
        } else {
            let mut fields = schema_property_order(schema);
            for key in input_obj.keys() {
                if !fields.iter().any(|f| f == key) {
                    fields.push(key.clone());
                }
            }
            for field in fields {
                push_archive_field_token(&mut tokens, input_obj, schema, &field);
            }
        }
    } else if !input.is_null() {
        tokens.push(match input {
            Value::String(s) => format!(
                "value={:?}",
                truncate_archive_string(s, ARCHIVE_ACTION_MAX_VALUE_CHARS)
            ),
            Value::Number(n) => format!("value={n}"),
            Value::Bool(b) => format!("value={b}"),
            Value::Array(a) => format!("value=[{} items]", a.len()),
            Value::Object(_) => "value={...}".to_string(),
            Value::Null => String::new(),
        });
    }

    if tokens.is_empty() {
        return op.to_string();
    }
    // Budget for the inner token list so the wrapped form stays balanced.
    // `op(` + `)` = op.chars().count() + 2; reserve one extra char for `…` if we have to elide.
    let op_chars = op.chars().count();
    let budget = ARCHIVE_ACTION_MAX_TOTAL_CHARS.saturating_sub(op_chars + 2);
    let inner = tokens.join(" ");
    if inner.chars().count() <= budget {
        format!("{op}({inner})")
    } else {
        // Reserve 1 char inside the parens for the ellipsis marker.
        let trimmed = truncate_archive_string(&inner, budget.saturating_sub(1));
        format!("{op}({trimmed})")
    }
}

#[async_trait]
pub trait ToolHandler: Send + Sync {
    fn metadata(&self) -> &ToolFunctionMetadata;
    fn capability(&self) -> ToolCapability {
        ToolCapability::OneShot
    }
    /// One-line natural language description of what the tool result contains.
    /// Used as the archive header summary and `tool_result` history item.
    /// Returns `None` when no meaningful description is available.
    fn describe_result_value(&self, _output: &Value) -> Option<String> {
        None
    }

    /// Produce a compact action/input identity for archive headers.
    ///
    /// Returns a string shaped like `Send(location_query="Paris")`. This should not include
    /// output details; it is only an affordance for choosing whether to read an existing `@N`.
    fn describe_archive_action(&self, op: &str, input: &Value) -> Option<String> {
        Some(project_archive_action_identity(
            op,
            input,
            Some(&self.metadata().input_schema),
        ))
    }

    /// Produce a natural language description of a tool action from its
    /// BAML-parsed result payload (the session step JSON). Dispatches on the
    /// FSM `op` field and deserialises the typed input where applicable.
    /// Every implementor must return a non-empty string; `Option` is intentionally
    /// absent so silent history deletion cannot occur.
    fn describe_invocation(&self, content: &Value) -> String {
        let step = content.get("step").unwrap_or(content);
        match step.get("op").and_then(Value::as_str) {
            Some(op) => format!("{}: {op}", self.metadata().name),
            None => String::new(),
        }
    }

    /// Semantic description of what opening this tool session means.
    /// Used by the drift gate to compare against the plan step intent
    /// *before* the session is actually opened.
    fn describe_open_action(&self) -> Option<String> {
        None
    }

    async fn open_session(
        &self,
        ctx: ToolSessionContext,
        open_input: Value,
    ) -> Result<Box<dyn ToolSession>>;

    /// Classify execution failures for this handler (installed on the session as
    /// [`ToolSessionContext::execution_classifier`] by the registry).
    ///
    /// Default uses [`ClassifiedToolError::from_baml_error`] (core-only disposition). [`ToolWrapper`]
    /// overrides to delegate to [`BamlTool::classify_execution_error`].
    fn classify_execution_error(&self, err: &BamlRtError) -> ClassifiedToolError {
        ClassifiedToolError::from_baml_error(err)
    }
}

pub trait ToolBundle: Send + Sync {
    fn metadata(&self) -> ToolBundleMetadata;
    fn functions(&self) -> Vec<Arc<dyn ToolHandler>>;
}

/// Registry for dynamically registered tool functions
pub struct ToolRegistry {
    inner: StdMutex<ToolRegistryInner>,
    sessions: DashMap<ToolSessionId, Arc<TokioMutex<Box<dyn ToolSession>>>>,
    /// Config version used when session was opened (for provenance linkage).
    session_config_version: DashMap<ToolSessionId, u64>,
}

struct ToolRegistryInner {
    tools: HashMap<ToolName, (ToolFunctionMetadata, Arc<dyn ToolHandler>)>,
    bundles: HashMap<BundleName, ToolBundleMetadata>,
    allowlist: Option<HashSet<ToolName>>,
    config_resolver: Option<Arc<dyn ConfigResolver>>,
}

/// Extract the tool name from a BAML-parsed result payload.
/// Handles two shapes:
/// - Direct: `{"tool_name": "support/notion", ...}`
/// - Wrapped variant: `{"SupportNotion": {"tool_name": "support/notion", ...}}`
fn extract_tool_name_from_payload(content: &Value) -> Option<String> {
    let obj = content.as_object()?;
    if let Some(name) = obj.get("tool_name").and_then(Value::as_str) {
        return Some(name.to_string());
    }
    if obj.len() == 1 {
        let (_, inner) = obj.iter().next()?;
        if let Some(name) = inner.get("tool_name").and_then(Value::as_str) {
            return Some(name.to_string());
        }
    }
    None
}

fn map_session_error(error: ToolSessionError) -> BamlRtError {
    match error {
        ToolSessionError::Transport(err) => err,
        ToolSessionError::Tool(failure) => tool_failure_to_baml_tool_execution_error(&failure),
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
        agent_id: &AgentId,
    ) -> Result<ToolSessionHandle<AwaitingInput>> {
        let session_id = registry
            .open_session(name, open_input, context_id, agent_id)
            .await?;
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

    pub async fn read(self, input: Value) -> Result<ToolSessionAdvance> {
        let registry = self.registry.clone();
        let registry_handle = self.registry.clone();
        let id = self.id.clone();
        let step = {
            let step = registry.session_read(&id, input).await?;
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
            let span = crate::spans::session_abort(&session_id, Some("session dropped"));
            handle.spawn(
                async move {
                    let reason = "session dropped";
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
                }
                .instrument(span),
            );
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
    .with_session_policy(T::SESSION_POLICY)
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

    fn describe_result_value(&self, output: &Value) -> Option<String> {
        let raw = output.get("output").unwrap_or(output);
        let typed: T::Output = serde_json::from_value(raw.clone()).ok()?;
        let desc = self.tool.describe_result(&typed);
        if desc.is_empty() { None } else { Some(desc) }
    }

    fn describe_archive_action(&self, op: &str, input: &Value) -> Option<String> {
        let raw = input.get("input").unwrap_or(input);
        match serde_json::from_value::<T::Input>(raw.clone()) {
            Ok(typed) => self
                .tool
                .action_identity(&typed)
                .map(|identity| {
                    project_archive_action_identity_from_parts(op, identity.name, identity.fields)
                })
                .or_else(|| {
                    Some(project_archive_action_identity(
                        op,
                        raw,
                        Some(&self.metadata.input_schema),
                    ))
                }),
            Err(err) => {
                tracing::debug!(
                    tool = %T::LOCAL_NAME,
                    op = %op,
                    error = %err,
                    "describe_archive_action: typed deserialize failed; falling back to schema-driven projection"
                );
                Some(project_archive_action_identity(
                    op,
                    raw,
                    Some(&self.metadata.input_schema),
                ))
            }
        }
    }

    fn describe_invocation(&self, content: &Value) -> String {
        // Single JSON parse point for the entire tool family.
        // Payload arrives in two shapes:
        //   wrapped:   {"step": {"op": "Send", "input": {...}}}
        //   unwrapped: {"op": "Send", "input": {...}}
        // After op extraction, the typed inputs are passed to the concrete tool's
        // ToolSession methods — no JSON parsing happens in those methods.
        let step = content
            .get("step")
            .and_then(|s| s.as_object())
            .map(|_| content.get("step").unwrap())
            .unwrap_or(content);
        let op = match step.get("op").and_then(Value::as_str) {
            Some(op) => op,
            None => return self.tool.describe_open(),
        };
        let input_val = step.get("input");
        match op {
            "Open" => input_val
                .and_then(|v| serde_json::from_value::<T::OpenInput>(v.clone()).ok())
                .map(|typed| self.tool.describe_open_input(&typed))
                .unwrap_or_else(|| self.tool.describe_open()),
            "Send" => input_val
                .and_then(|v| serde_json::from_value::<T::Input>(v.clone()).ok())
                .map(|typed| self.tool.describe_invocation(&typed))
                .unwrap_or_else(|| format!("{}: send", self.metadata().name)),
            // SearchRead/PageRead show up as SessionSteps with archive content; Finish/Abort are terminal.
            // Neither adds semantic value as a ToolCall entry in the LLM's conversation history.
            "SearchRead" | "PageRead" | "Finish" | "Abort" => String::new(),
            other => format!("{}: {other}", self.metadata().name),
        }
    }

    fn describe_open_action(&self) -> Option<String> {
        Some(self.tool.describe_open())
    }

    fn classify_execution_error(&self, err: &BamlRtError) -> ClassifiedToolError {
        T::classify_execution_error(err)
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
    /// Session context; source of the execution classifier used by send/read ops.
    ctx: ToolSessionContext,
    executor: Box<dyn ToolExecutor>,
    last_send: Option<Value>,
}

impl MultiSendSession {
    fn new(ctx: ToolSessionContext, executor: Box<dyn ToolExecutor>) -> Self {
        Self {
            ctx,
            executor,
            last_send: None,
        }
    }
}

/// Session that allows Send followed by one or more Read calls. Send sets session scope and each
/// Read executes the scoped query with read-time refinement input.
#[async_trait]
impl ToolSession for MultiSendSession {
    async fn send(&mut self, input: Value) -> std::result::Result<(), ToolSessionError> {
        self.last_send = Some(input);
        Ok(())
    }

    async fn read(&mut self, input: Value) -> std::result::Result<ToolStep, ToolSessionError> {
        let mut merged = match self.last_send.clone() {
            Some(Value::Object(map)) => Value::Object(map),
            Some(v) => v,
            None => Value::Object(serde_json::Map::new()),
        };
        if let (Value::Object(base), Value::Object(refine)) = (&mut merged, input) {
            for (k, v) in refine {
                base.insert(k, v);
            }
        }
        let output = match self.executor.execute(merged).await {
            Ok(value) => value,
            Err(err) => {
                return Err(ToolSessionError::Tool(ToolFailure::from_error_in_session(
                    &self.ctx.execution_classifier,
                    &err,
                )));
            }
        };
        Ok(ToolStep::Done {
            output: Some(output),
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
                config_resolver: None,
            }),
            sessions: DashMap::new(),
            session_config_version: DashMap::new(),
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

    /// Set the config resolver for session open. When set and the tool has config metadata,
    /// config is resolved and passed to the session context.
    pub fn set_config_resolver(&self, resolver: Option<Arc<dyn ConfigResolver>>) {
        let mut inner = self.inner.lock().unwrap();
        inner.config_resolver = resolver;
    }

    /// Register a tool that implements the BamlTool trait
    ///
    /// # Arguments
    /// * `tool` - An instance of a type implementing `BamlTool`
    ///
    /// # Example
    /// ```rust,no_run
    /// use baml_rt_tools::{BamlTool, DescribeAction, Support, ToolRegistry};
    /// use baml_rt_core::Result;
    /// use serde::{Deserialize, Serialize};
    /// use baml_derive::BamlType;
    /// use async_trait::async_trait;
    ///
    /// struct MyTool;
    ///
    /// #[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
    /// struct MyInput {}
    ///
    /// #[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
    /// struct MyOutput {}
    ///
    /// impl DescribeAction for MyInput {
    ///     fn describe(&self) -> String {
    ///         "my_tool".to_string()
    ///     }
    /// }
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

    /// Get tool metadata by parsed `ToolName` — no re-parsing overhead.
    pub fn get_metadata_by_name(&self, name: &ToolName) -> Option<ToolFunctionMetadata> {
        let inner = self.inner.lock().unwrap();
        inner.tools.get(name).map(|(metadata, _)| metadata.clone())
    }

    /// Get a registered tool handler by name.
    pub fn get_handler(&self, name: &str) -> Option<Arc<dyn ToolHandler>> {
        let parsed = ToolName::parse(name).ok()?;
        let inner = self.inner.lock().unwrap();
        inner.tools.get(&parsed).map(|(_, handler)| handler.clone())
    }

    /// Produce a natural language description of a tool-action payload,
    /// routed by tool name to the correct handler. No trial deserialization.
    pub fn describe_invocation_for(&self, tool_name: &str, content: &Value) -> Option<String> {
        let parsed = ToolName::parse(tool_name).ok()?;
        let inner = self.inner.lock().unwrap();
        let (_, handler) = inner.tools.get(&parsed)?;
        Some(handler.describe_invocation(content))
    }

    /// Get a compact action/input identity for an archive header.
    pub fn describe_archive_action_for(
        &self,
        tool_name: &str,
        op: &str,
        input: &Value,
    ) -> Option<String> {
        let parsed = ToolName::parse(tool_name).ok()?;
        let inner = self.inner.lock().unwrap();
        let (_, handler) = inner.tools.get(&parsed)?;
        handler.describe_archive_action(op, input)
    }

    /// Get a one-line description of what a tool result contains.
    /// Used to populate the archive header summary at `ToolStep::Done`.
    pub fn describe_result_for(&self, tool_name: &str, output: &Value) -> Option<String> {
        let parsed = ToolName::parse(tool_name).ok()?;
        let inner = self.inner.lock().unwrap();
        let (_, handler) = inner.tools.get(&parsed)?;
        handler.describe_result_value(output)
    }

    /// Get the `describe_open()` text for a tool by name.
    /// Used by the drift gate to produce the action description before opening a session.
    pub fn describe_open_for(&self, tool_name: &str) -> Option<String> {
        let parsed = ToolName::parse(tool_name).ok()?;
        let inner = self.inner.lock().unwrap();
        let (_, handler) = inner.tools.get(&parsed)?;
        handler.describe_open_action()
    }

    /// Try to extract the tool name from a BAML-parsed result payload
    /// (looks for `tool_name` or `{variant}.tool_name` fields), then route
    /// to the named handler's `describe_invocation`.
    pub fn describe_invocation(&self, content: &Value) -> Option<String> {
        let tool_name = extract_tool_name_from_payload(content)?;
        self.describe_invocation_for(&tool_name, content)
    }

    /// Describe an invocation by tool name from the LlmEffectMetadata,
    /// falling back to payload extraction if the tool name isn't provided.
    /// Returns the handler's description when a matching tool is registered.
    /// Returns empty string when no tool matches — callers that need a non-empty
    /// value must provide their own fallback. Empty string is explicitly preferred
    /// over a synthetic "{tool}: call" fallback because that leaks meaningless text
    /// into provenance, drift scoring, and UI.
    pub fn describe_invocation_with_hint(
        &self,
        tool_name_hint: Option<&str>,
        content: &Value,
    ) -> String {
        let from_hint = tool_name_hint.and_then(|n| self.describe_invocation_for(n, content));
        let from_payload = || self.describe_invocation(content);
        from_hint.or_else(from_payload).unwrap_or_default()
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
    /// `context_id` and `agent_id` come from the invocation scope.
    pub async fn open_session(
        &self,
        name: &str,
        open_input: Value,
        context_id: &ContextId,
        agent_id: &AgentId,
    ) -> Result<ToolSessionId> {
        self.open_session_scoped(name, open_input, context_id, agent_id, None)
            .await
    }

    /// Open a tool session and return its session id with explicit optional task scope.
    pub async fn open_session_scoped(
        &self,
        name: &str,
        open_input: Value,
        context_id: &ContextId,
        agent_id: &AgentId,
        task_id: Option<&TaskId>,
    ) -> Result<ToolSessionId> {
        let start = std::time::Instant::now();
        let parsed = ToolName::parse(name)?;
        let session_id = ToolSessionId::random();

        let result = async {
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

            let (config, config_version) = {
                let resolver = self.inner.lock().unwrap().config_resolver.clone();
                let config_key = metadata.config_bundle.as_ref();
                match (resolver, &metadata.config, config_key) {
                    (Some(r), Some(config_meta), Some(bundle_name)) => {
                        let opt = r
                            .get_config_with_version(bundle_name)
                            .await
                            .ok()
                            .flatten()
                            .or_else(|| Some((config_meta.default.clone(), 0u64)));
                        opt.map(|(v, ver)| (Some(v), Some(ver)))
                            .unwrap_or((None, None))
                    }
                    _ => (metadata.config.as_ref().map(|m| m.default.clone()), None),
                }
            };

            let handler_for_classify = handler.clone();
            let execution_classifier: ToolExecutionClassifier =
                Arc::new(move |err| handler_for_classify.classify_execution_error(err));
            let ctx = ToolSessionContext {
                session_id: session_id.clone(),
                tool_name: metadata.name.clone(),
                context_id: context_id.clone(),
                agent_id: agent_id.clone(),
                config,
                config_version,
                task_id: task_id.cloned(),
                execution_classifier: Some(execution_classifier),
            };
            let session = handler.open_session(ctx, open_input).await?;
            self.sessions
                .insert(session_id.clone(), Arc::new(TokioMutex::new(session)));
            if let Some(ver) = config_version {
                self.session_config_version.insert(session_id.clone(), ver);
            }

            Ok(())
        }
        .instrument(crate::spans::open_session(&session_id, &parsed))
        .await;

        let duration = start.elapsed();
        crate::metrics::record_session_open(&parsed.to_string());
        crate::metrics::record_session_operation("open", duration);

        result?;
        Ok(session_id)
    }

    pub async fn session_send(&self, session_id: &ToolSessionId, input: Value) -> Result<()> {
        let start = std::time::Instant::now();

        let result = async {
            let session = self
                .sessions
                .get(session_id)
                .map(|entry| entry.value().clone());
            let session = session.ok_or_else(|| {
                if tool_registry_trace_enabled() {
                    tool_registry_trace(&format!(
                        "session_send missing: session_id={}, known_sessions={}",
                        session_id,
                        self.sessions.len()
                    ));
                }
                BamlRtError::SessionLifecycle(SessionLifecycleError::ToolSessionNotFound {
                    session_id: session_id.to_string(),
                })
            })?;
            let mut guard = session.lock().await;
            guard.send(input).await.map_err(map_session_error)
        }
        .instrument(crate::spans::session_send(session_id))
        .await;

        let duration = start.elapsed();
        crate::metrics::record_session_operation("send", duration);

        result
    }

    pub async fn session_read(&self, session_id: &ToolSessionId, input: Value) -> Result<ToolStep> {
        let start = std::time::Instant::now();

        let result = async {
            let session = self
                .sessions
                .get(session_id)
                .map(|entry| entry.value().clone());
            let session = session.ok_or_else(|| {
                if tool_registry_trace_enabled() {
                    tool_registry_trace(&format!(
                        "session_read missing: session_id={}, known_sessions={}",
                        session_id,
                        self.sessions.len()
                    ));
                }
                BamlRtError::SessionLifecycle(SessionLifecycleError::ToolSessionNotFound {
                    session_id: session_id.to_string(),
                })
            })?;
            let mut guard = session.lock().await;
            guard.read(input).await.map_err(map_session_error)
        }
        .instrument(crate::spans::session_read(session_id))
        .await;

        let duration = start.elapsed();
        crate::metrics::record_session_operation("read", duration);

        result
    }

    pub async fn session_finish(&self, session_id: &ToolSessionId) -> Result<()> {
        let start = std::time::Instant::now();

        let result = async {
            self.session_config_version.remove(session_id);
            let session = self.sessions.remove(session_id).map(|(_, session)| session);
            if let Some(session) = session {
                let mut guard = session.lock().await;
                guard.finish().await.map_err(map_session_error)?;
            }
            Ok(())
        }
        .instrument(crate::spans::session_finish(session_id))
        .await;

        let duration = start.elapsed();
        crate::metrics::record_session_operation("finish", duration);

        result
    }

    pub async fn session_abort(
        &self,
        session_id: &ToolSessionId,
        reason: Option<String>,
    ) -> Result<()> {
        let start = std::time::Instant::now();

        let span = crate::spans::session_abort(session_id, reason.as_deref());
        let result = async {
            self.session_config_version.remove(session_id);
            let session = self.sessions.remove(session_id).map(|(_, session)| session);
            if let Some(session) = session {
                let mut guard = session.lock().await;
                guard.abort(reason).await.map_err(map_session_error)?;
            }
            Ok(())
        }
        .instrument(span)
        .await;

        let duration = start.elapsed();
        crate::metrics::record_session_operation("abort", duration);

        result
    }

    /// Config version used when this session was opened (for provenance linkage).
    pub fn get_config_version_for_session(&self, session_id: &ToolSessionId) -> Option<u64> {
        self.session_config_version.get(session_id).map(|r| *r)
    }

    /// Execute a tool function by name (single-shot convenience). `context_id` and `agent_id` from invocation scope.
    pub async fn execute(
        &self,
        name: &str,
        args: Value,
        context_id: &ContextId,
        agent_id: &AgentId,
    ) -> Result<Value> {
        async {
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
                .open_session(
                    &parsed.to_string(),
                    empty_open_input(),
                    context_id,
                    agent_id,
                )
                .await?;
            self.session_send(&session_id, args).await?;
            match self.session_read(&session_id, Value::Null).await? {
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
            }
        }
        .instrument(crate::spans::execute_tool(name))
        .await
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

/// One-shot tool built from an async function and pre-built metadata.
///
/// Open validates `OI`, Send captures `I`, and the first Read executes the handler
/// and returns a single `Done` payload.
pub fn create_one_shot_tool_from_async<OI, I, O, F>(
    metadata: ToolFunctionMetadata,
    executor: F,
) -> Arc<dyn ToolHandler>
where
    OI: crate::tool_schema::ToolType
        + Serialize
        + for<'de> Deserialize<'de>
        + DescribeAction
        + Send
        + Sync
        + 'static,
    I: crate::tool_schema::ToolType
        + Serialize
        + for<'de> Deserialize<'de>
        + DescribeAction
        + Send
        + Sync
        + 'static,
    O: crate::tool_schema::ToolType + Serialize + Send + Sync + 'static,
    F: Fn(I) -> Pin<Box<dyn Future<Output = Result<O>> + Send>> + Send + Sync + 'static,
{
    Arc::new(OneShotToolFromAsync::<OI, I, O, F> {
        metadata,
        executor: Arc::new(executor),
        _phantom: PhantomData,
    })
}

/// One-shot tool built from an async function and pre-built metadata, with access
/// to the session context used to open the tool.
pub fn create_one_shot_tool_from_async_with_context<OI, I, O, F>(
    metadata: ToolFunctionMetadata,
    executor: F,
) -> Arc<dyn ToolHandler>
where
    OI: crate::tool_schema::ToolType
        + Serialize
        + for<'de> Deserialize<'de>
        + DescribeAction
        + Send
        + Sync
        + 'static,
    I: crate::tool_schema::ToolType
        + Serialize
        + for<'de> Deserialize<'de>
        + DescribeAction
        + Send
        + Sync
        + 'static,
    O: crate::tool_schema::ToolType + Serialize + Send + Sync + 'static,
    F: Fn(ToolSessionContext, I) -> Pin<Box<dyn Future<Output = Result<O>> + Send>>
        + Send
        + Sync
        + 'static,
{
    Arc::new(OneShotToolFromAsyncWithContext::<OI, I, O, F> {
        metadata,
        executor: Arc::new(executor),
        _phantom: PhantomData,
    })
}

struct OneShotToolFromAsync<OI, I, O, F> {
    metadata: ToolFunctionMetadata,
    executor: Arc<F>,
    _phantom: PhantomData<(OI, I, O)>,
}

struct OneShotToolFromAsyncWithContext<OI, I, O, F> {
    metadata: ToolFunctionMetadata,
    executor: Arc<F>,
    _phantom: PhantomData<(OI, I, O)>,
}

#[async_trait]
impl<OI, I, O, F> ToolHandler for OneShotToolFromAsync<OI, I, O, F>
where
    OI: crate::tool_schema::ToolType
        + Serialize
        + for<'de> Deserialize<'de>
        + DescribeAction
        + Send
        + Sync
        + 'static,
    I: crate::tool_schema::ToolType
        + Serialize
        + for<'de> Deserialize<'de>
        + DescribeAction
        + Send
        + Sync
        + 'static,
    O: crate::tool_schema::ToolType + Serialize + Send + Sync + 'static,
    F: Fn(I) -> Pin<Box<dyn Future<Output = Result<O>> + Send>> + Send + Sync + 'static,
{
    fn metadata(&self) -> &ToolFunctionMetadata {
        &self.metadata
    }

    fn describe_invocation(&self, content: &Value) -> String {
        let step = content.get("step").unwrap_or(content);
        let tool_name = self.metadata.name.to_string();
        let op = match step.get("op").and_then(Value::as_str) {
            Some(op) => op,
            None => return format!("using {tool_name}"),
        };
        match op {
            "Open" => step
                .get("input")
                .and_then(|v| serde_json::from_value::<OI>(v.clone()).ok())
                .map(|typed| typed.describe())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("using {tool_name}")),
            "Send" => step
                .get("input")
                .and_then(|v| serde_json::from_value::<I>(v.clone()).ok())
                .map(|typed| typed.describe())
                .unwrap_or_else(|| format!("{tool_name}: send")),
            "SearchRead" | "PageRead" | "Finish" | "Abort" => String::new(),
            other => format!("{tool_name}: {other}"),
        }
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
        Ok(Box::new(OneShotSession::new(ctx, executor)))
    }
}

#[async_trait]
impl<OI, I, O, F> ToolHandler for OneShotToolFromAsyncWithContext<OI, I, O, F>
where
    OI: crate::tool_schema::ToolType
        + Serialize
        + for<'de> Deserialize<'de>
        + DescribeAction
        + Send
        + Sync
        + 'static,
    I: crate::tool_schema::ToolType
        + Serialize
        + for<'de> Deserialize<'de>
        + DescribeAction
        + Send
        + Sync
        + 'static,
    O: crate::tool_schema::ToolType + Serialize + Send + Sync + 'static,
    F: Fn(ToolSessionContext, I) -> Pin<Box<dyn Future<Output = Result<O>> + Send>>
        + Send
        + Sync
        + 'static,
{
    fn metadata(&self) -> &ToolFunctionMetadata {
        &self.metadata
    }

    fn describe_invocation(&self, content: &Value) -> String {
        let step = content.get("step").unwrap_or(content);
        let tool_name = self.metadata.name.to_string();
        let op = match step.get("op").and_then(Value::as_str) {
            Some(op) => op,
            None => return format!("using {tool_name}"),
        };
        match op {
            "Open" => step
                .get("input")
                .and_then(|v| serde_json::from_value::<OI>(v.clone()).ok())
                .map(|typed| typed.describe())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("using {tool_name}")),
            "Send" => step
                .get("input")
                .and_then(|v| serde_json::from_value::<I>(v.clone()).ok())
                .map(|typed| typed.describe())
                .unwrap_or_else(|| format!("{tool_name}: send")),
            "SearchRead" | "PageRead" | "Finish" | "Abort" => String::new(),
            other => format!("{tool_name}: {other}"),
        }
    }

    async fn open_session(
        &self,
        ctx: ToolSessionContext,
        open_input: Value,
    ) -> Result<Box<dyn ToolSession>> {
        validate_open_input::<OI>(open_input)?;

        let session_ctx = ctx.clone();
        let handler = self.executor.clone();
        let executor: Box<dyn ToolExecutor> = Box::new(ExecutorAdapter::new(move |input| {
            let parsed: I = match deserialize_tool_input(input) {
                Ok(value) => value,
                Err(err) => return Box::pin(async move { Err(err) }),
            };
            let future = handler(session_ctx.clone(), parsed);
            Box::pin(async move {
                let output = future.await?;
                serialize_tool_output(output)
            })
        }));
        Ok(Box::new(OneShotSession::new(ctx, executor)))
    }
}

/// Session tool with Send/Read pairs, built from an async function and pre-built metadata.
///
/// Send establishes scope/state for a retrieval window. Read performs explicit traversal within
/// that window and returns `ToolStep` output. Tools may implement streaming/suspension semantics in Read.
///
/// Open validates OI; Send sets scope and Read executes scoped retrieval. Use when you have
/// runtime deps and want multi-request sessions without implementing [ToolHandler].
pub fn create_multi_send_session_tool_from_async<OI, I, O, F>(
    metadata: ToolFunctionMetadata,
    executor: F,
) -> Arc<dyn ToolHandler>
where
    OI: for<'de> Deserialize<'de> + DescribeAction + Send + Sync + 'static,
    I: crate::tool_schema::ToolType
        + Serialize
        + for<'de> Deserialize<'de>
        + DescribeAction
        + Send
        + Sync
        + 'static,
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
    OI: for<'de> Deserialize<'de> + DescribeAction + Send + Sync + 'static,
    I: crate::tool_schema::ToolType
        + Serialize
        + for<'de> Deserialize<'de>
        + DescribeAction
        + Send
        + Sync
        + 'static,
    O: crate::tool_schema::ToolType + Serialize + Send + Sync + 'static,
    F: Fn(I) -> Pin<Box<dyn Future<Output = Result<O>> + Send>> + Send + Sync + 'static,
{
    fn metadata(&self) -> &ToolFunctionMetadata {
        &self.metadata
    }

    fn describe_invocation(&self, content: &Value) -> String {
        // Accept both wrapped { step: { op, input } } and unwrapped { op, input }.
        let step = content.get("step").unwrap_or(content);
        let tool_name = self.metadata.name.to_string();
        let op = match step.get("op").and_then(Value::as_str) {
            Some(op) => op,
            None => return format!("using {tool_name}"),
        };
        match op {
            "Open" => step
                .get("input")
                .and_then(|v| serde_json::from_value::<OI>(v.clone()).ok())
                .map(|oi| oi.describe())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("using {tool_name}")),
            "Send" => step
                .get("input")
                .and_then(|v| serde_json::from_value::<I>(v.clone()).ok())
                .map(|typed| typed.describe())
                .unwrap_or_else(|| format!("{tool_name}: send")),
            "SearchRead" | "PageRead" | "Finish" | "Abort" => String::new(),
            other => format!("{tool_name}: {other}"),
        }
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
    send_input: Option<Value>,
    state: SessionPhase,
}

impl OneShotSession {
    fn new(ctx: ToolSessionContext, executor: Box<dyn ToolExecutor>) -> Self {
        Self {
            ctx,
            executor,
            send_input: None,
            state: SessionPhase::Open,
        }
    }
}

#[async_trait]
impl ToolSession for OneShotSession {
    async fn send(&mut self, input: Value) -> std::result::Result<(), ToolSessionError> {
        if self.send_input.is_some() {
            return Err(ToolSessionError::Tool(ToolFailure::invalid_input(
                "Tool session already has input",
            )));
        }
        self.send_input = Some(input);
        Ok(())
    }

    async fn read(&mut self, input: Value) -> std::result::Result<ToolStep, ToolSessionError> {
        if self.state.is_closed() {
            return Ok(ToolStep::Done { output: None });
        }
        let send_input = self.send_input.take().ok_or_else(|| {
            ToolSessionError::Tool(ToolFailure::invalid_input(format!(
                "Tool session {} has no input",
                self.ctx.session_id
            )))
        })?;
        let mut merged = send_input;
        if let (Value::Object(base), Value::Object(refine)) = (&mut merged, input) {
            for (k, v) in refine {
                base.insert(k, v);
            }
        }
        let output = match self.executor.execute(merged).await {
            Ok(value) => value,
            Err(err) => {
                return Ok(ToolStep::Error {
                    error: ToolFailure::from_error_in_session(&self.ctx.execution_classifier, &err),
                });
            }
        };
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

#[cfg(test)]
mod tool_config_metadata_tests {
    use serde_json::json;

    use super::{ToolConfigMetadata, Value};

    #[test]
    fn default_from_schema_builds_object_from_property_defaults() {
        let schema = json!({
            "type": "object",
            "properties": {
                "api_key": { "type": "string", "default": "env.NOTION_API_KEY" },
                "base_url": { "type": "string", "default": "https://api.notion.com" }
            }
        });
        let got = ToolConfigMetadata::default_from_schema(&schema).unwrap();
        let obj = got.as_object().unwrap();
        assert_eq!(
            obj.get("api_key").and_then(Value::as_str),
            Some("env.NOTION_API_KEY")
        );
        assert_eq!(
            obj.get("base_url").and_then(Value::as_str),
            Some("https://api.notion.com")
        );
    }

    #[test]
    fn default_from_schema_none_when_no_properties_defaults() {
        let schema = json!({ "type": "object", "properties": { "x": { "type": "string" } } });
        assert!(ToolConfigMetadata::default_from_schema(&schema).is_none());
    }

    /// Metadata requires a default at construction; no optional default. Explicit value is stored as-is.
    #[test]
    fn explicit_default_is_stored() {
        let schema =
            json!({ "type": "object", "properties": { "k": { "default": "from_schema" } } });
        let meta = ToolConfigMetadata {
            schema: schema.clone(),
            default: json!({ "k": "explicit" }),
            type_name: None,
        };
        assert_eq!(meta.default, json!({ "k": "explicit" }));
    }

    /// When building from a schema with property defaults, use default_from_schema and pass the result (or json!({})) so default is never optional.
    #[test]
    fn default_from_schema_supplies_required_default() {
        let schema =
            json!({ "type": "object", "properties": { "k": { "default": "from_schema" } } });
        let default = ToolConfigMetadata::default_from_schema(&schema).unwrap();
        let meta = ToolConfigMetadata {
            schema: schema.clone(),
            default: default.clone(),
            type_name: None,
        };
        assert_eq!(meta.default, json!({ "k": "from_schema" }));
    }
}

#[cfg(test)]
mod session_plan_type_name_tests {
    use super::*;

    #[test]
    fn new_accepts_valid_suffix() {
        let name = SessionPlanTypeName::new("SupportCalculateSessionPlan").unwrap();
        assert_eq!(name.as_str(), "SupportCalculateSessionPlan");
        assert_eq!(name.class_name(), "SupportCalculate");
    }

    #[test]
    fn new_rejects_missing_suffix() {
        let result = SessionPlanTypeName::new("SupportCalculate");
        assert!(result.is_err());
    }

    #[test]
    fn new_rejects_empty_string() {
        let result = SessionPlanTypeName::new("");
        assert!(result.is_err());
    }

    #[test]
    fn class_name_strips_suffix() {
        let name = SessionPlanTypeName::new("SystemInternalA2aSessionPlan").unwrap();
        assert_eq!(name.class_name(), "SystemInternalA2a");
    }

    #[test]
    fn display_matches_inner() {
        let name = SessionPlanTypeName::new("FooSessionPlan").unwrap();
        assert_eq!(format!("{name}"), "FooSessionPlan");
    }

    #[test]
    fn serde_roundtrip() {
        let name = SessionPlanTypeName::new("XSessionPlan").unwrap();
        let json = serde_json::to_string(&name).unwrap();
        assert_eq!(json, "\"XSessionPlan\"");
        let back: SessionPlanTypeName = serde_json::from_str(&json).unwrap();
        assert_eq!(back, name);
    }

    #[test]
    fn serde_rejects_invalid_on_deserialize() {
        let result = serde_json::from_str::<SessionPlanTypeName>("\"NotAPlan\"");
        assert!(result.is_err());
    }

    #[test]
    fn session_plan_functions_map_serde_roundtrip() {
        let mut map = SessionPlanFunctionsMap::new();
        map.insert(
            "ChooseAction".to_string(),
            vec![
                SessionPlanTypeName::new("SupportCalcSessionPlan").unwrap(),
                SessionPlanTypeName::new("SystemA2aSessionPlan").unwrap(),
            ],
        );
        let json = serde_json::to_string(&map).unwrap();
        let back: SessionPlanFunctionsMap = serde_json::from_str(&json).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back["ChooseAction"].len(), 2);
    }
}

#[cfg(test)]
mod session_type_names_alignment_tests {
    use baml_rt_core::BamlFunctionId;

    use super::{SessionTypeNames, ToolName};

    #[test]
    fn entry_active_consume_round_trip_parse() {
        let base = "ExecuteStep";
        let tn = ToolName::parse("support/calculate").unwrap();
        let slug = tn.slug();

        for name in [
            SessionTypeNames::entry(base),
            SessionTypeNames::active(base, &slug),
            SessionTypeNames::consume(base, &slug),
        ] {
            let id = BamlFunctionId::parse(&name);
            assert_eq!(id.full_name(), name, "parse round-trip for {name}");
        }
    }

    #[test]
    fn legacy_select_act_continue_still_parse() {
        let id = BamlFunctionId::parse("ExecuteStep__select");
        assert!(matches!(
            id.phase(),
            Some(baml_rt_core::VariantPhase::Select)
        ));
        let id = BamlFunctionId::parse("ExecuteStep__act__support_calculate");
        assert!(matches!(
            id.phase(),
            Some(baml_rt_core::VariantPhase::Act { .. })
        ));
        let id = BamlFunctionId::parse("ExecuteStep__continue__support_calculate");
        assert!(matches!(
            id.phase(),
            Some(baml_rt_core::VariantPhase::Continue { .. })
        ));
    }
}

#[cfg(test)]
mod tool_name_identifier_tests {
    use super::{
        BundleName, LocalToolName, ToolFunctionMetadata, ToolName, parse_tool_name_and_class,
    };

    #[test]
    fn accepts_ascii_lowercase_identifier_components() {
        assert!(BundleName::new("internal-dev").is_ok());
        assert!(BundleName::new("internal_dev").is_ok());
        assert!(LocalToolName::new("get_weather2").is_ok());
        assert!(ToolName::parse("internal-dev/get_weather2").is_ok());
        assert!(ToolName::parse("internal_dev/get_weather2").is_ok());
    }

    #[test]
    fn rejects_non_ascii_or_uppercase_identifier_components() {
        assert!(BundleName::new("Support").is_err());
        assert!(LocalToolName::new("echo🙂").is_err());
        assert!(ToolName::parse("support/echo🙂").is_err());
    }

    #[test]
    fn derive_class_name_normalizes_hyphen_and_underscore() {
        let tool_name = ToolName::parse("internal-dev/internal_a2a").unwrap();
        let class_name =
            ToolFunctionMetadata::derive_class_name(tool_name.bundle(), tool_name.local());
        assert_eq!(class_name, "InternalDevInternalA2a");
    }

    #[test]
    fn parse_tool_name_and_class_normalizes_external_style_ids() {
        let (_tool_name, class_name) = parse_tool_name_and_class("dev/meteo-tool").unwrap();
        assert_eq!(class_name, "DevMeteoTool");
    }

    #[test]
    fn tool_slug_normalizes_hyphen_to_underscore() {
        let tool_name = ToolName::parse("dev/meteo-tool").unwrap();
        assert_eq!(tool_name.slug().as_str(), "dev_meteo_tool");
    }
}

#[cfg(test)]
mod archive_identity_tests {
    use serde_json::json;

    use super::{project_archive_action_identity, project_archive_action_identity_from_parts};

    #[test]
    fn flat_schema_projects_present_non_default_fields() {
        let schema = json!({
            "type": "object",
            "properties": {
                "location_query": { "type": "string" },
                "timezone": { "type": "string", "default": "auto" },
                "api_key": { "type": "string" }
            }
        });
        let input = json!({
            "location_query": "Paris, France",
            "timezone": "auto",
            "api_key": "secret"
        });
        assert_eq!(
            project_archive_action_identity("Send", &input, Some(&schema)),
            "Send(location_query=\"Paris, France\" api_key=<redacted>)"
        );
    }

    #[test]
    fn tagged_one_of_skips_non_matching_branches_to_find_match() {
        // Regression: prior code used `?` inside the branch loop, which returned None
        // from the whole function as soon as the first branch failed shape/const checks.
        let schema = json!({
            "oneOf": [
                {
                    "required": ["action"],
                    "properties": {
                        "action": { "const": "first_action" },
                        "irrelevant": { "type": "string" }
                    }
                },
                {
                    "required": ["action", "list_id"],
                    "properties": {
                        "action": { "const": "list_tasks" },
                        "list_id": { "type": "string" }
                    }
                }
            ]
        });
        let input = json!({ "action": "list_tasks", "list_id": "lst_42" });
        assert_eq!(
            project_archive_action_identity("Send", &input, Some(&schema)),
            "Send(list_tasks list_id=\"lst_42\")"
        );
    }

    #[test]
    fn long_input_truncation_keeps_balanced_parens() {
        // Regression: prior code truncated the wrapped string and dropped the closing `)`.
        let schema = json!({
            "type": "object",
            "properties": { "blob": { "type": "string" } }
        });
        let big = "x".repeat(500);
        let input = json!({ "blob": big });
        let out = project_archive_action_identity("Send", &input, Some(&schema));
        assert!(out.starts_with("Send("), "must start with op(: {out}");
        assert!(out.ends_with(')'), "must end with ): {out}");
        assert!(out.contains('…'), "expected ellipsis on truncation: {out}");
        assert!(
            out.chars().count() <= 96,
            "must respect total char cap: {} > 96",
            out.chars().count()
        );
    }

    #[test]
    fn tagged_one_of_projects_action_and_identity_fields() {
        let schema = json!({
            "oneOf": [
                {
                    "required": ["action", "list_id"],
                    "properties": {
                        "action": { "const": "list_tasks" },
                        "list_id": { "type": "string" }
                    }
                }
            ]
        });
        let input = json!({ "action": "list_tasks", "list_id": "lst_42" });
        assert_eq!(
            project_archive_action_identity("Send", &input, Some(&schema)),
            "Send(list_tasks list_id=\"lst_42\")"
        );
    }

    #[test]
    fn typed_identity_parts_cap_fields_and_mark_omission() {
        let out = project_archive_action_identity_from_parts(
            "Send",
            Some("create_task"),
            vec![
                ("list_id", json!("lst_42")),
                ("name", json!("Fix bug")),
                ("priority", json!(1)),
                (
                    "description",
                    json!("long details that should not be shown"),
                ),
            ],
        );
        assert_eq!(
            out,
            "Send(create_task list_id=\"lst_42\" name=\"Fix bug\" priority=1 …)"
        );
    }

    #[test]
    fn typed_identity_parts_still_redact_sensitive_fields() {
        let out = project_archive_action_identity_from_parts(
            "Send",
            Some("call_api"),
            vec![("api_key", json!("secret")), ("query", json!("status"))],
        );
        assert_eq!(out, "Send(call_api api_key=<redacted> query=\"status\")");
    }
}

#[cfg(test)]
mod unified_step_executor_authoring_parse_tests {
    use super::parse_unified_step_executors_authoring_json;

    #[test]
    fn parses_roots_wrapper() {
        let text = r#"{"roots":{"PlanFoo":{"include_archive_reads":false}}}"#;
        let m = parse_unified_step_executors_authoring_json(text);
        let cfg = m.get("PlanFoo").expect("PlanFoo key");
        assert!(!cfg.include_archive_reads);
    }

    #[test]
    fn parses_flat_object_map() {
        let text = r#"{"PlanBar":{}}"#;
        let m = parse_unified_step_executors_authoring_json(text);
        let cfg = m.get("PlanBar").expect("PlanBar key");
        assert!(cfg.include_archive_reads);
    }
}
