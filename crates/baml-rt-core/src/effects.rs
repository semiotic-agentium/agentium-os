//! Effect system: primary source of truth for tool/LLM execution events.
//!
//! Effects drive provenance (provenance is a subscriber), and liveness gating uses
//! in-flight effect counts to distinguish "waiting on effect" from "will never yield".

use crate::ids::ContextId;
use async_trait::async_trait;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Arc;
use tokio::sync::RwLock;

/// What kind of effect is being executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectKind {
    Tool,
    Llm,
    A2a,
}

impl EffectKind {
    /// Message for underflow logging (Completed without matching Started).
    fn underflow_message(&self) -> &'static str {
        match self {
            EffectKind::Tool => {
                "Effect count underflow: ToolCompleted without matching ToolStarted"
            }
            EffectKind::Llm => "Effect count underflow: LlmCompleted without matching LlmStarted",
            EffectKind::A2a => "Effect count underflow: A2aCompleted without matching A2aStarted",
        }
    }
}

/// Marker types for effect kind (used in typestate tokens).
pub struct ToolKind;
pub struct LlmKind;
pub struct A2aKind;

/// Effect metadata for tool calls (needed for provenance).
#[derive(Debug, Clone)]
pub struct ToolEffectMetadata {
    pub tool_name: String,
    pub function_name: Option<String>,
    pub args: serde_json::Value,
    pub metadata: serde_json::Value,
}

/// Effect metadata for LLM calls (needed for provenance).
#[derive(Debug, Clone)]
pub struct LlmEffectMetadata {
    pub client: String,
    pub model: String,
    pub function_name: String,
    pub prompt: serde_json::Value,
    pub metadata: serde_json::Value,
}

impl ToolEffectMetadata {
    /// Validate that metadata contains message_id (for provenance requirements).
    ///
    /// This provides runtime validation of metadata capability requirements.
    /// In the future, this could be encoded via phantom types for compile-time guarantees.
    pub fn has_message_id(&self) -> bool {
        self.metadata
            .get("message_id")
            .and_then(|v| v.as_str())
            .is_some()
    }
}

impl LlmEffectMetadata {
    /// Validate that metadata contains message_id (for provenance requirements).
    ///
    /// This provides runtime validation of metadata capability requirements.
    /// In the future, this could be encoded via phantom types for compile-time guarantees.
    pub fn has_message_id(&self) -> bool {
        self.metadata
            .get("message_id")
            .and_then(|v| v.as_str())
            .is_some()
    }
}

/// Effect metadata for A2A calls (host-inbound).
#[derive(Debug, Clone)]
pub struct A2aEffectMetadata {
    pub agent_id: crate::ids::AgentId,
    pub method: String,
    pub request_id: Option<String>,
    pub metadata: serde_json::Value,
}

/// Effect lifecycle event: started or completed.
#[derive(Debug, Clone)]
pub enum EffectEvent {
    ToolStarted {
        context_id: ContextId,
        metadata: ToolEffectMetadata,
    },
    ToolCompleted {
        context_id: ContextId,
        metadata: ToolEffectMetadata,
        duration_ms: u64,
        success: bool,
    },
    LlmStarted {
        context_id: ContextId,
        metadata: LlmEffectMetadata,
    },
    LlmCompleted {
        context_id: ContextId,
        metadata: LlmEffectMetadata,
        usage: Option<LlmUsage>,
        duration_ms: u64,
        success: bool,
    },
    A2aStarted {
        context_id: ContextId,
        metadata: A2aEffectMetadata,
    },
    A2aCompleted {
        context_id: ContextId,
        metadata: A2aEffectMetadata,
        duration_ms: u64,
        success: bool,
    },
}

/// LLM usage information (matches provenance).
#[derive(Debug, Clone)]
pub enum LlmUsage {
    Known {
        prompt_tokens: u64,
        completion_tokens: u64,
        total_tokens: u64,
    },
    Unknown,
}

impl EffectEvent {
    pub fn kind(&self) -> EffectKind {
        match self {
            EffectEvent::ToolStarted { .. } | EffectEvent::ToolCompleted { .. } => EffectKind::Tool,
            EffectEvent::LlmStarted { .. } | EffectEvent::LlmCompleted { .. } => EffectKind::Llm,
            EffectEvent::A2aStarted { .. } | EffectEvent::A2aCompleted { .. } => EffectKind::A2a,
        }
    }

    pub fn context_id(&self) -> &ContextId {
        match self {
            EffectEvent::ToolStarted { context_id, .. }
            | EffectEvent::ToolCompleted { context_id, .. } => context_id,
            EffectEvent::LlmStarted { context_id, .. }
            | EffectEvent::LlmCompleted { context_id, .. } => context_id,
            EffectEvent::A2aStarted { context_id, .. }
            | EffectEvent::A2aCompleted { context_id, .. } => context_id,
        }
    }
}

/// Typestate token representing a started effect.
///
/// **CG3 / E1 (Effect Token Completion):** The token must be completed via `complete()` or
/// explicitly abandoned. If dropped without completion, effect counts leak. Drop logs an error
/// and panics in debug builds. Fields are `Option` so `complete()` can take them without
/// triggering drop of the token.
pub struct EffectStartToken<K> {
    context_id: Option<ContextId>,
    metadata: Option<EffectStartMetadata>,
    _kind: PhantomData<K>,
}

/// Internal metadata storage for start tokens.
enum EffectStartMetadata {
    Tool(ToolEffectMetadata),
    Llm(LlmEffectMetadata),
    A2a(A2aEffectMetadata),
}

/// DRY: take context_id and metadata from token fields (used by complete() impls).
fn take_token_parts(
    context_id: &mut Option<ContextId>,
    metadata: &mut Option<EffectStartMetadata>,
) -> (ContextId, EffectStartMetadata) {
    (
        context_id.take().expect("token already completed"),
        metadata.take().expect("token already completed"),
    )
}

impl<K> Drop for EffectStartToken<K> {
    fn drop(&mut self) {
        if self.context_id.is_some() || self.metadata.is_some() {
            #[cfg(debug_assertions)]
            {
                tracing::error!(
                    context_id = ?self.context_id,
                    kind = std::any::type_name::<K>(),
                    "EffectStartToken dropped without completion - effect leak (CG3 violation)"
                );
                panic!(
                    "EffectStartToken dropped without completion: context_id={:?}, kind={}",
                    self.context_id,
                    std::any::type_name::<K>()
                );
            }
            #[cfg(not(debug_assertions))]
            {
                tracing::error!(
                    context_id = ?self.context_id,
                    kind = std::any::type_name::<K>(),
                    "EffectStartToken dropped without completion - effect leak (CG3 violation)"
                );
            }
        }
    }
}

impl EffectStartToken<ToolKind> {
    /// Complete a tool effect. Consumes the token (Drop will not run).
    pub async fn complete(
        mut self,
        emitter: &dyn EffectEmitter,
        duration_ms: u64,
        success: bool,
    ) -> crate::Result<()> {
        let (context_id, metadata) = take_token_parts(&mut self.context_id, &mut self.metadata);
        let metadata = match metadata {
            EffectStartMetadata::Tool(meta) => meta,
            _ => unreachable!(),
        };
        emitter
            .emit(EffectEvent::ToolCompleted {
                context_id,
                metadata,
                duration_ms,
                success,
            })
            .await
    }
}

impl EffectStartToken<LlmKind> {
    /// Complete an LLM effect. Consumes the token (Drop will not run).
    pub async fn complete(
        mut self,
        emitter: &dyn EffectEmitter,
        usage: Option<LlmUsage>,
        duration_ms: u64,
        success: bool,
    ) -> crate::Result<()> {
        let (context_id, metadata) = take_token_parts(&mut self.context_id, &mut self.metadata);
        let metadata = match metadata {
            EffectStartMetadata::Llm(meta) => meta,
            _ => unreachable!(),
        };
        emitter
            .emit(EffectEvent::LlmCompleted {
                context_id,
                metadata,
                usage,
                duration_ms,
                success,
            })
            .await
    }
}

impl EffectStartToken<A2aKind> {
    /// Complete an A2A effect. Consumes the token (Drop will not run).
    pub async fn complete(
        mut self,
        emitter: &dyn EffectEmitter,
        duration_ms: u64,
        success: bool,
    ) -> crate::Result<()> {
        let (context_id, metadata) = take_token_parts(&mut self.context_id, &mut self.metadata);
        let metadata = match metadata {
            EffectStartMetadata::A2a(meta) => meta,
            _ => unreachable!(),
        };
        emitter
            .emit(EffectEvent::A2aCompleted {
                context_id,
                metadata,
                duration_ms,
                success,
            })
            .await
    }
}

/// Emitter trait: executors call this to declare effect start/complete.
#[async_trait]
pub trait EffectEmitter: Send + Sync {
    /// Legacy emit method (for backward compatibility during migration).
    async fn emit(&self, event: EffectEvent) -> crate::Result<()>;

    /// Start a tool effect and return a token that must be completed.
    async fn start_tool(
        &self,
        context_id: ContextId,
        metadata: ToolEffectMetadata,
    ) -> crate::Result<EffectStartToken<ToolKind>> {
        let token = EffectStartToken {
            context_id: Some(context_id.clone()),
            metadata: Some(EffectStartMetadata::Tool(metadata.clone())),
            _kind: PhantomData,
        };
        self.emit(EffectEvent::ToolStarted {
            context_id,
            metadata,
        })
        .await?;
        Ok(token)
    }

    /// Start an LLM effect and return a token that must be completed.
    async fn start_llm(
        &self,
        context_id: ContextId,
        metadata: LlmEffectMetadata,
    ) -> crate::Result<EffectStartToken<LlmKind>> {
        let token = EffectStartToken {
            context_id: Some(context_id.clone()),
            metadata: Some(EffectStartMetadata::Llm(metadata.clone())),
            _kind: PhantomData,
        };
        self.emit(EffectEvent::LlmStarted {
            context_id,
            metadata,
        })
        .await?;
        Ok(token)
    }

    /// Start an A2A effect and return a token that must be completed.
    async fn start_a2a(
        &self,
        context_id: ContextId,
        metadata: A2aEffectMetadata,
    ) -> crate::Result<EffectStartToken<A2aKind>> {
        let token = EffectStartToken {
            context_id: Some(context_id.clone()),
            metadata: Some(EffectStartMetadata::A2a(metadata.clone())),
            _kind: PhantomData,
        };
        self.emit(EffectEvent::A2aStarted {
            context_id,
            metadata,
        })
        .await?;
        Ok(token)
    }
}

/// Liveness query: check if any effects are in-flight for a context.
#[async_trait]
pub trait EffectLiveness: Send + Sync {
    /// Get in-flight counts for a context (tool + LLM).
    async fn in_flight(&self, context_id: &ContextId) -> InFlightCounts;
}

/// In-flight counts per context (context-only scope).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InFlightCounts {
    pub tool: u32,
    pub llm: u32,
    pub a2a: u32,
}

impl InFlightCounts {
    pub fn any(&self) -> bool {
        self.tool > 0 || self.llm > 0 || self.a2a > 0
    }

    pub fn total(&self) -> u32 {
        self.tool + self.llm + self.a2a
    }

    /// Mutable reference to the count for the given effect kind (for DRY count updates).
    pub fn get_mut(&mut self, kind: EffectKind) -> &mut u32 {
        match kind {
            EffectKind::Tool => &mut self.tool,
            EffectKind::Llm => &mut self.llm,
            EffectKind::A2a => &mut self.a2a,
        }
    }
}

/// Effect bus: maintains in-flight index and fans out to subscribers.
pub struct EffectBus {
    counts: Arc<RwLock<HashMap<ContextId, InFlightCounts>>>,
    subscribers: Arc<RwLock<Vec<Arc<dyn EffectSubscriber>>>>,
}

/// Subscriber trait: receives effect events (e.g. provenance adapter).
#[async_trait]
pub trait EffectSubscriber: Send + Sync {
    async fn on_effect(&self, event: &EffectEvent) -> crate::Result<()>;
}

impl EffectBus {
    pub fn new() -> Self {
        Self {
            counts: Arc::new(RwLock::new(HashMap::new())),
            subscribers: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Add a subscriber (e.g. provenance adapter).
    pub async fn subscribe(&self, subscriber: Arc<dyn EffectSubscriber>) {
        let mut subs = self.subscribers.write().await;
        subs.push(subscriber);
    }

    /// Update counts and notify subscribers.
    async fn process_event(&self, event: EffectEvent) -> crate::Result<()> {
        let context_id = event.context_id().clone();
        let kind = event.kind();

        // Update in-flight counts (DRY: single path per Started/Completed via EffectKind)
        {
            let mut counts = self.counts.write().await;
            let entry = counts
                .entry(context_id.clone())
                .or_insert_with(InFlightCounts::default);
            match event {
                EffectEvent::ToolStarted { .. }
                | EffectEvent::LlmStarted { .. }
                | EffectEvent::A2aStarted { .. } => {
                    let count = entry.get_mut(kind);
                    *count = count.saturating_add(1);
                }
                EffectEvent::ToolCompleted { .. }
                | EffectEvent::LlmCompleted { .. }
                | EffectEvent::A2aCompleted { .. } => {
                    let count = entry.get_mut(kind);
                    if *count == 0 {
                        tracing::error!(context_id = ?context_id, "{}", kind.underflow_message());
                    }
                    *count = count.saturating_sub(1);
                    if !entry.any() {
                        counts.remove(&context_id);
                    }
                }
            }
        }

        // Fan out to subscribers
        let subs = self.subscribers.read().await;
        for sub in subs.iter() {
            if let Err(e) = sub.on_effect(&event).await {
                tracing::warn!(error = ?e, "Effect subscriber failed");
            }
        }

        Ok(())
    }
}

impl Default for EffectBus {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EffectEmitter for EffectBus {
    async fn emit(&self, event: EffectEvent) -> crate::Result<()> {
        self.process_event(event).await
    }
}

#[async_trait]
impl EffectLiveness for EffectBus {
    async fn in_flight(&self, context_id: &ContextId) -> InFlightCounts {
        let counts = self.counts.read().await;
        counts.get(context_id).copied().unwrap_or_default()
    }
}
