//! Sandbox-backed [`ToolInvoker`] + lifetime cache.
//!
//! Maps the trait surface from Workstream A to the sandbox world:
//! `describe` / `invoke` are JSON-RPC 2.0 frames carried over the
//! provider-supplied [`TsrpcChannel`] (§5.2), and the same cache key scheme
//! from §9.2 (`(agent_id, context_id, tool_name)`) lives here.
//!
//! Lifetime behavior:
//! - Lazy create on first `invoke` (§9.4 "lazy first-use").
//! - In-process reattach via [`SandboxProvider::reattach`] with the §9.4
//!   validation checklist.
//! - Eviction on `SandboxTerminatedUnexpectedly` — next invoke cold-creates.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use baml_rt_core::{BamlRtError, ContextId, Result, ids::AgentId};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tracing::{debug, warn};

use super::{
    provider::SandboxProvider,
    spec::{SandboxHandle, SandboxSpec},
};
use crate::{
    ToolName,
    external_tools::{
        invoker::{InvokeRequest, InvokeResponse, ToolDescribe, ToolInvoker},
        protocol::{
            JsonRpcRequest, JsonRpcResponse, METHOD_DESCRIBE, METHOD_INVOKE, ToolDescribeResult,
            ToolInvokeParams, ToolInvokeResult,
        },
    },
};

/// Cache key from §9.2. `runner_id` is layered on via [`SandboxCache`] — it
/// scopes the entire cache, not individual entries.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SandboxCacheKey {
    pub agent_id: AgentId,
    pub context_id: ContextId,
    pub tool_name: ToolName,
}

/// Factory producing a fresh [`SandboxSpec`] for a `(tool_name, key)` pair.
///
/// The runtime owns this — it is where metadata, resolved secrets, and the
/// effective [`NetworkPolicy`](super::spec::NetworkPolicy) get composed.
/// Workstream D will grow the real implementation; for now the runtime wires
/// a simple closure and the invoker treats it as opaque.
pub type SandboxSpecBuilder =
    Arc<dyn Fn(&SandboxCacheKey) -> Result<SandboxSpec> + Send + Sync + 'static>;

/// Runtime-owned sandbox cache (§9.2–§9.4).
pub struct SandboxCache {
    runner_id: String,
    entries: Mutex<HashMap<SandboxCacheKey, SandboxHandle>>,
}

impl SandboxCache {
    pub fn new(runner_id: impl Into<String>) -> Self {
        Self {
            runner_id: runner_id.into(),
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub fn runner_id(&self) -> &str {
        &self.runner_id
    }

    /// Encode a microsandbox-safe name for the cache key.
    ///
    /// Name must remain short enough for underlying UNIX socket path limits
    /// (`SUN_LEN`). We keep a small runner-scoped prefix + stable hash.
    pub fn encode_name(&self, key: &SandboxCacheKey) -> String {
        let runner = sanitize_component(&self.runner_id, 8);
        let hash = short_hash(&format!(
            "{}|{}|{}|{}",
            self.runner_id, key.agent_id, key.context_id, key.tool_name
        ));
        format!("baml:{runner}:{hash}")
    }

    /// Look up or lazily create a handle. `reattach_ok` toggles whether to
    /// try `provider.reattach(name)` before cold-creating (hot reload path).
    pub async fn get_or_create(
        &self,
        provider: &dyn SandboxProvider,
        build_spec: &SandboxSpecBuilder,
        key: &SandboxCacheKey,
        reattach_ok: bool,
    ) -> Result<SandboxHandle> {
        {
            let entries = self.entries.lock().unwrap();
            if let Some(handle) = entries.get(key)
                && !handle.is_expired()
            {
                return Ok(handle.clone());
            }
        }

        let spec = build_spec(key)?;
        let name = self.encode_name(key);
        if spec.name != name {
            return Err(BamlRtError::InvalidArgument(format!(
                "SandboxSpec.name '{}' does not match encoded name '{}'",
                spec.name, name
            )));
        }

        let reattach_ok = reattach_ok && spec.image.is_oci();

        if reattach_ok && let Ok(existing) = provider.reattach(&name).await {
            if validate_reattach(&existing, &spec) {
                debug!(sandbox = %name, "reattached to existing sandbox");
                self.entries
                    .lock()
                    .unwrap()
                    .insert(key.clone(), existing.clone());
                return Ok(existing);
            } else {
                warn!(
                    sandbox = %name,
                    "reattach validation failed — tearing down + cold-creating"
                );
                let _ = provider.teardown(&existing).await;
            }
        }

        let handle = provider.create(spec).await?;
        self.entries
            .lock()
            .unwrap()
            .insert(key.clone(), handle.clone());
        Ok(handle)
    }

    /// Evict a cache entry (e.g. on `SandboxTerminatedUnexpectedly`).
    pub fn evict(&self, key: &SandboxCacheKey) -> Option<SandboxHandle> {
        self.entries.lock().unwrap().remove(key)
    }

    /// Remove every cached entry. Caller is responsible for tearing down
    /// the returned handles via the provider.
    pub fn drain(&self) -> Vec<SandboxHandle> {
        let mut entries = self.entries.lock().unwrap();
        entries.drain().map(|(_, h)| h).collect()
    }

    /// Current warm entry count — surfaced for the `tool_sandbox_active`
    /// gauge defined in §14.2.
    pub fn active_count(&self) -> usize {
        self.entries.lock().unwrap().len()
    }
}

/// §9.4 reattach validation checklist.
///
/// - **Policy hash match** — if both sides declare one, they must agree.
/// - **Age check** — handle must still have lifetime left.
///
/// Status / context-liveness checks from the full list belong upstream (the
/// runtime knows its active contexts; the provider knows runtime status).
/// Workstream B wires the two checks we can evaluate here.
fn validate_reattach(handle: &SandboxHandle, spec: &SandboxSpec) -> bool {
    if handle.is_expired() {
        return false;
    }
    if let (Some(h), Some(s)) = (&handle.policy_hash, &spec.policy_hash)
        && h != s
    {
        return false;
    }
    if handle.guest_workdir != spec.guest_workdir {
        return false;
    }
    true
}

/// [`ToolInvoker`] that routes every call through a [`SandboxProvider`] and
/// the shared [`SandboxCache`].
///
/// A single invoker instance serves one `(agent_id, context_id)` scope —
/// callers construct one per scope so the cache key lookup is O(tool) at
/// `invoke` time.
pub struct SandboxInvoker {
    provider: Arc<dyn SandboxProvider>,
    cache: Arc<SandboxCache>,
    build_spec: SandboxSpecBuilder,
    agent_id: AgentId,
    context_id: ContextId,
    describe_timeout: Duration,
    invoke_timeout: Duration,
}

impl SandboxInvoker {
    pub fn new(
        provider: Arc<dyn SandboxProvider>,
        cache: Arc<SandboxCache>,
        build_spec: SandboxSpecBuilder,
        agent_id: AgentId,
        context_id: ContextId,
    ) -> Self {
        Self {
            provider,
            cache,
            build_spec,
            agent_id,
            context_id,
            describe_timeout: Duration::from_secs(10),
            invoke_timeout: Duration::from_secs(120),
        }
    }

    pub fn with_timeouts(mut self, describe: Duration, invoke: Duration) -> Self {
        self.describe_timeout = describe;
        self.invoke_timeout = invoke;
        self
    }

    fn key(&self, tool: &ToolName) -> SandboxCacheKey {
        SandboxCacheKey {
            agent_id: self.agent_id.clone(),
            context_id: self.context_id.clone(),
            tool_name: tool.clone(),
        }
    }

    async fn json_rpc_call(
        &self,
        tool: &ToolName,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let key = self.key(tool);
        let handle = self
            .cache
            .get_or_create(&*self.provider, &self.build_spec, &key, true)
            .await?;
        let mut channel = self.provider.rpc_channel(&handle).await?;
        let request = JsonRpcRequest::new(method, rand_id(), params);
        let payload =
            serde_json::to_value(&request).map_err(|e| BamlRtError::InvalidArgumentWithSource {
                message: format!("failed to encode JSON-RPC request for {method}"),
                source: Box::new(e),
            })?;
        let call = async {
            channel
                .send(&payload)
                .await
                .map_err(|e| BamlRtError::InvalidArgumentWithSource {
                    message: format!("failed to send TSRPC frame for {method}"),
                    source: Box::new(e),
                })?;
            channel
                .recv()
                .await
                .map_err(|e| BamlRtError::InvalidArgumentWithSource {
                    message: format!("failed to recv TSRPC frame for {method}"),
                    source: Box::new(e),
                })
        };
        let value = tokio::time::timeout(timeout, call).await.map_err(|_| {
            BamlRtError::InvalidArgument(format!(
                "sandbox invoke timed out after {:?} for method {method}",
                timeout
            ))
        })??;
        let response: JsonRpcResponse =
            serde_json::from_value(value).map_err(|e| BamlRtError::InvalidArgumentWithSource {
                message: "failed to decode JSON-RPC response envelope".to_string(),
                source: Box::new(e),
            })?;
        if let Some(err) = response.error {
            // Reuse the process path's error-class mapping so sandbox and
            // process backends surface identical classifications upstream
            // — crucial for the §13 failure taxonomy.
            return Err(crate::external_tools::invoker::map_jsonrpc_error(
                tool, &err,
            ));
        }
        response.result.ok_or_else(|| {
            BamlRtError::InvalidArgument("JSON-RPC response missing result".to_string())
        })
    }
}

fn sanitize_component(input: &str, max_len: usize) -> String {
    let mut out = String::with_capacity(max_len);
    for ch in input.chars() {
        let mapped = if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            ch
        } else {
            '_'
        };
        out.push(mapped);
        if out.len() >= max_len {
            break;
        }
    }
    if out.is_empty() { "x".to_string() } else { out }
}

fn short_hash(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let digest = hasher.finalize();
    let hex = format!("{:x}", digest);
    hex[..10].to_string()
}

fn rand_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[async_trait]
impl ToolInvoker for SandboxInvoker {
    async fn describe(&self, tool: &ToolName, timeout: Duration) -> Result<ToolDescribe> {
        let params = json!({});
        let value = self
            .json_rpc_call(
                tool,
                METHOD_DESCRIBE,
                params,
                timeout.min(self.describe_timeout),
            )
            .await?;
        let parsed: ToolDescribeResult =
            serde_json::from_value(value).map_err(|e| BamlRtError::InvalidArgumentWithSource {
                message: "failed to decode tool/describe result".to_string(),
                source: Box::new(e),
            })?;
        Ok(parsed.into())
    }

    async fn invoke(&self, req: InvokeRequest) -> Result<InvokeResponse> {
        let params = ToolInvokeParams {
            invocation_id: req.invocation_id,
            tool_name: req.tool_name.to_string(),
            input: req.input,
            secrets: req.secrets,
            capabilities: req.capabilities,
        };
        let params =
            serde_json::to_value(&params).map_err(|e| BamlRtError::InvalidArgumentWithSource {
                message: "failed to encode tool/invoke params".to_string(),
                source: Box::new(e),
            })?;
        let timeout = req.timeout.min(self.invoke_timeout);
        let value = self
            .json_rpc_call(&req.tool_name, METHOD_INVOKE, params, timeout)
            .await?;
        let parsed: ToolInvokeResult =
            serde_json::from_value(value).map_err(|e| BamlRtError::InvalidArgumentWithSource {
                message: "failed to decode tool/invoke result".to_string(),
                source: Box::new(e),
            })?;
        Ok(InvokeResponse {
            output: parsed.output,
            done: parsed.done,
        })
    }
}
