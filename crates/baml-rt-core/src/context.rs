//! Context ID propagation for async invocation flows.
//!
//! This module provides task-local context and a **type-enforced** invocation scope:
//! scope is constructed once at request entry (e.g. transport) and threaded through
//! the pipeline. No fallback to generate context_id—construction and passing are
//! controlled by types.
//!
//! Runtime context must be threaded explicitly at runtime boundaries.
//! Missing scope is a failure condition; APIs return `Result`, not `Option`.

use std::{
    ops::Deref,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use baml_rt_id::ExternalId;

use crate::{
    error::Result,
    ids::{AgentId, ContextId, MessageId, TaskId},
};

/// Error when no invocation scope is set (e.g. not running inside `with_scope`).
pub const NO_SCOPE_MESSAGE: &str =
    "No invocation scope set. Run inside context::with_scope(scope, ...) (e.g. from transport).";

/// Trait for obtaining the current invocation scope. Runtime context is exposed only through
/// this interface. Missing scope is a failure; use `scope()?` or handle the error.
pub trait InvocationContext {
    /// Returns the current invocation scope when running inside `with_scope(scope, ...)`.
    /// Returns `Err` when no scope is set—downstream does not have to handle optionality.
    fn scope(&self) -> Result<RuntimeScope>;
}

/// Wrapper that carries a reference and an invocation scope. Use when an API must run
/// with a specific scope (e.g. tool execution, session send/next). Implements
/// [`InvocationContext`] so scope-dependent code can call `.scope()` and get
/// the stored scope without touching task-local or globals.
#[derive(Debug, Clone)]
pub struct Scoped<'a, T> {
    pub inner: &'a T,
    pub scope: RuntimeScope,
}

impl<'a, T> Scoped<'a, T> {
    pub fn new(inner: &'a T, scope: RuntimeScope) -> Self {
        Self { inner, scope }
    }
}

impl<T> InvocationContext for Scoped<'_, T> {
    fn scope(&self) -> Result<RuntimeScope> {
        Ok(self.scope.clone())
    }
}

/// Request-level scope (no agent_id). Built from parsed A2A request; used to construct
/// RuntimeScope once agent_id is known. Makes message-scoped vs task-scoped explicit.
///
/// Resume must use `RequestScope::TaskScoped`; `RuntimeScope::from_request_scope(resolved_scope, agent_id)`
/// is the single source for BAML and conversation history in that turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestScope {
    MessageScoped {
        context_id: ContextId,
        message_id: MessageId,
    },
    TaskScoped {
        context_id: ContextId,
        message_id: MessageId,
        task_id: TaskId,
    },
}

impl RequestScope {
    pub fn context_id(&self) -> &ContextId {
        match self {
            Self::MessageScoped { context_id, .. } | Self::TaskScoped { context_id, .. } => {
                context_id
            }
        }
    }

    pub fn message_id(&self) -> &MessageId {
        match self {
            Self::MessageScoped { message_id, .. } | Self::TaskScoped { message_id, .. } => {
                message_id
            }
        }
    }

    pub fn task_id_opt(&self) -> Option<&TaskId> {
        match self {
            Self::MessageScoped { .. } => None,
            Self::TaskScoped { task_id, .. } => Some(task_id),
        }
    }
}

/// How this A2A outcome request is being invoked. Drives scope resolution without Option.
#[derive(Debug, Clone)]
pub enum OutcomeInvocationContext {
    /// Request not from a live stream session; use parsed request's resolved_scope only.
    Standalone,
    /// Live session, first turn: context_id from session; task_id derived so SUBMITTED is emitted and drain can set session_task_id.
    LiveSessionFirstTurn { context_id: ContextId },
    /// Live session, resume: context_id and task_id from previous drain.
    LiveSessionResume {
        context_id: ContextId,
        task_id: TaskId,
    },
}

/// Discriminated union for invocation scope: message-level or task-level.
/// No standalone variant; context kind is explicit in the type.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuntimeScope {
    MessageScope {
        context_id: ContextId,
        agent_id: AgentId,
        message_id: MessageId,
    },
    TaskScope {
        context_id: ContextId,
        agent_id: AgentId,
        message_id: MessageId,
        task_id: TaskId,
    },
}

impl RuntimeScope {
    /// Build scope for a message-level invocation (no task).
    pub fn message_scope(context_id: ContextId, agent_id: AgentId, message_id: MessageId) -> Self {
        Self::MessageScope {
            context_id,
            agent_id,
            message_id,
        }
    }

    /// Build scope for a task-level invocation.
    pub fn task_scope(
        context_id: ContextId,
        agent_id: AgentId,
        message_id: MessageId,
        task_id: TaskId,
    ) -> Self {
        Self::TaskScope {
            context_id,
            agent_id,
            message_id,
            task_id,
        }
    }

    pub fn context_id(&self) -> &ContextId {
        match self {
            Self::MessageScope { context_id, .. } | Self::TaskScope { context_id, .. } => {
                context_id
            }
        }
    }

    pub fn agent_id(&self) -> &AgentId {
        match self {
            Self::MessageScope { agent_id, .. } | Self::TaskScope { agent_id, .. } => agent_id,
        }
    }

    pub fn message_id(&self) -> &MessageId {
        match self {
            Self::MessageScope { message_id, .. } | Self::TaskScope { message_id, .. } => {
                message_id
            }
        }
    }

    pub fn task_id_opt(&self) -> Option<&TaskId> {
        match self {
            Self::MessageScope { .. } => None,
            Self::TaskScope { task_id, .. } => Some(task_id),
        }
    }

    /// Build RuntimeScope from request-level scope and agent_id (set at transport).
    pub fn from_request_scope(scope: &RequestScope, agent_id: AgentId) -> Self {
        let context_id = scope.context_id().clone();
        let message_id = scope.message_id().clone();
        match scope {
            RequestScope::MessageScoped { .. } => {
                Self::message_scope(context_id, agent_id, message_id)
            }
            RequestScope::TaskScoped { task_id, .. } => {
                Self::task_scope(context_id, agent_id, message_id, task_id.clone())
            }
        }
    }
}

/// Type-enforced invocation scope: the only way to run scope-dependent operations.
///
/// **Construction:** Build once at request entry (e.g. A2A transport from parsed request).
/// **Passing:** Thread explicitly through route → invoker → bridge. Do not call `require_scope()`
/// or default to generating context_id; the type system enforces that scope is passed in.
///
/// Use [`InvocationScope::new`] with a `RuntimeScope` built from the request; then pass
/// `&InvocationScope` (or clone for async boundaries) to every layer that runs under that scope.
#[derive(Debug, Clone)]
pub struct InvocationScope(pub RuntimeScope);

impl InvocationScope {
    /// Build an invocation scope from the request's runtime scope. Call only at the top
    /// of the pipeline (e.g. transport); then pass this value through.
    pub fn new(scope: RuntimeScope) -> Self {
        Self(scope)
    }

    /// Synthetic message scope for CLI/test only. Uses generated context_id and message_id.
    /// Do not use in runtime request paths.
    pub fn synthetic_message(agent_id: AgentId) -> Self {
        let counter = CONTEXT_COUNTER.fetch_add(1, Ordering::Relaxed);
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let context_id = ContextId::new(millis, counter);
        let message_id = MessageId::from_external(ExternalId::new(format!("syn-msg-{}", counter)));
        Self(RuntimeScope::message_scope(
            context_id, agent_id, message_id,
        ))
    }

    /// Synthetic task scope for CLI/test only. Uses generated context_id, message_id, and task_id.
    /// Do not use in runtime request paths.
    pub fn synthetic_task(agent_id: AgentId) -> Self {
        let counter = CONTEXT_COUNTER.fetch_add(1, Ordering::Relaxed);
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let context_id = ContextId::new(millis, counter);
        let message_id = MessageId::from_external(ExternalId::new(format!("syn-msg-{}", counter)));
        let task_id = TaskId::from_external(ExternalId::new(format!("syn-task-{}", counter)));
        Self(RuntimeScope::task_scope(
            context_id, agent_id, message_id, task_id,
        ))
    }

    /// Access the underlying scope (e.g. for `with_scope(self.0.clone(), ...)`).
    pub fn as_scope(&self) -> &RuntimeScope {
        &self.0
    }
}

impl Deref for InvocationScope {
    type Target = RuntimeScope;
    fn deref(&self) -> &RuntimeScope {
        &self.0
    }
}

tokio::task_local! {
    static RUNTIME_SCOPE: RuntimeScope;
}

static CONTEXT_COUNTER: AtomicU64 = AtomicU64::new(1);

pub fn generate_context_id() -> ContextId {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let counter = CONTEXT_COUNTER.fetch_add(1, Ordering::Relaxed);
    ContextId::new(millis, counter)
}

pub async fn with_scope<F, T>(scope: RuntimeScope, fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    RUNTIME_SCOPE.scope(scope, fut).await
}

/// Returns the current runtime scope when running inside `with_scope`.
/// Errors when no scope is set.
pub fn current_scope() -> Result<RuntimeScope> {
    RUNTIME_SCOPE
        .try_with(|scope| scope.clone())
        .map_err(|_| crate::error::BamlRtError::InvalidArgument(NO_SCOPE_MESSAGE.to_string()))
}
