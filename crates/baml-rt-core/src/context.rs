//! Context ID propagation for async invocation flows.
//!
//! This module provides task-local context and a **type-enforced** invocation scope:
//! scope is constructed once at request entry (e.g. transport) and threaded through
//! the pipeline. No fallback to generate context_id—construction and passing are
//! controlled by types.
//!
//! **Runtime context is exposed only via the [`InvocationContext`] trait.** Code that
//! needs the current scope must use a type implementing that trait (e.g. [`task_local_context()`]).
//! Missing scope is a failure condition; the trait returns `Result`, not `Option`.

use crate::error::{BamlRtError, Result};
use crate::ids::{AgentId, ContextId, MessageId, TaskId};
use std::ops::Deref;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Error when no invocation scope is set (e.g. not running inside `with_scope`).
pub const NO_SCOPE_MESSAGE: &str =
    "No invocation scope set. Run inside context::with_scope(scope, ...) (e.g. from transport).";

/// Trait for obtaining the current invocation scope. Runtime context is exposed only through
/// this interface. Missing scope is a failure; use `current_scope()?` or handle the error.
pub trait InvocationContext {
    /// Returns the current invocation scope when running inside `with_scope(scope, ...)`.
    /// Returns `Err` when no scope is set—downstream does not have to handle optionality.
    fn current_scope(&self) -> Result<RuntimeScope>;
}

/// Task-local invocation context: reads the scope from the tokio task-local set by
/// [`with_scope`]. Use [`task_local_context()`] to obtain an instance.
#[derive(Debug, Clone, Copy)]
pub struct TaskLocalContext;

impl InvocationContext for TaskLocalContext {
    fn current_scope(&self) -> Result<RuntimeScope> {
        require_scope()
    }
}

/// Returns the task-local invocation context. Use this when you need to pass an
/// [`InvocationContext`] to code that requires the current scope (e.g. `open_tool_session`).
/// Scope is set by running inside [`with_scope`](with_scope) (e.g. from the transport).
pub fn task_local_context() -> TaskLocalContext {
    TaskLocalContext
}

/// Wrapper that carries a reference and an invocation scope. Use when an API must run
/// with a specific scope (e.g. tool execution, session send/next). Implements
/// [`InvocationContext`] so scope-dependent code can call `.current_scope()` and get
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
    fn current_scope(&self) -> Result<RuntimeScope> {
        Ok(self.scope.clone())
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeScope {
    pub context_id: ContextId,
    pub agent_id: AgentId,
    pub message_id: Option<MessageId>,
    pub task_id: Option<TaskId>,
}

impl RuntimeScope {
    pub fn new(
        context_id: ContextId,
        agent_id: AgentId,
        message_id: Option<MessageId>,
        task_id: Option<TaskId>,
    ) -> Self {
        Self {
            context_id,
            agent_id,
            message_id,
            task_id,
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

    /// Build a standalone scope for CLI/test or other non-request paths that still need
    /// to run scope-dependent JS (e.g. direct invoke_js_function). Uses a generated
    /// context_id and no message/task ids.
    pub fn standalone(agent_id: AgentId) -> Self {
        Self(RuntimeScope::new(
            generate_context_id(),
            agent_id,
            None,
            None,
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

/// Requires the task-local scope; not public. Use [`InvocationContext::current_scope`] via [`task_local_context()`] instead.
/// Missing scope is a failure—returns `Err`, no optionality for downstream.
pub(crate) fn require_scope() -> Result<RuntimeScope> {
    RUNTIME_SCOPE
        .try_with(|scope| scope.clone())
        .map_err(|_| BamlRtError::InvalidArgument(NO_SCOPE_MESSAGE.to_string()))
}

/// Read the current context_id when running inside `with_scope(scope, ...)`.
pub fn current_context_id() -> Option<ContextId> {
    task_local_context()
        .current_scope()
        .ok()
        .map(|scope| scope.context_id)
}

pub fn current_agent_id() -> Option<AgentId> {
    task_local_context()
        .current_scope()
        .ok()
        .map(|scope| scope.agent_id)
}

pub fn current_message_id() -> Option<MessageId> {
    task_local_context()
        .current_scope()
        .ok()
        .and_then(|scope| scope.message_id)
}

pub fn current_task_id() -> Option<TaskId> {
    task_local_context()
        .current_scope()
        .ok()
        .and_then(|scope| scope.task_id)
}

/// Context ID for request-entry paths (e.g. store) when no scope is set. Prefer
/// requiring scope and using [`InvocationContext::current_scope`] in runtime/tool paths.
pub fn context_id_or_generated() -> ContextId {
    current_context_id().unwrap_or_else(generate_context_id)
}

pub async fn with_scope<F, T>(scope: RuntimeScope, fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    RUNTIME_SCOPE.scope(scope, fut).await
}

pub async fn with_context_id<F, T>(id: ContextId, fut: F) -> Result<T>
where
    F: std::future::Future<Output = T>,
{
    let mut scope = task_local_context().current_scope()?;
    scope.context_id = id.clone();
    Ok(with_scope(scope, fut).await)
}

pub async fn with_message_id<F, T>(id: MessageId, fut: F) -> Result<T>
where
    F: std::future::Future<Output = T>,
{
    let scope = task_local_context().current_scope()?;
    let scope = RuntimeScope::new(scope.context_id, scope.agent_id, Some(id), scope.task_id);
    Ok(with_scope(scope, fut).await)
}

pub async fn with_task_id<F, T>(id: TaskId, fut: F) -> Result<T>
where
    F: std::future::Future<Output = T>,
{
    let scope = task_local_context().current_scope()?;
    let scope = RuntimeScope::new(scope.context_id, scope.agent_id, scope.message_id, Some(id));
    Ok(with_scope(scope, fut).await)
}

/// Run `fut` with the current scope's context_id/message_id/task_id but with
/// `agent_id` set to `id`. Fails if no invocation scope is set (no implicit scope creation).
pub async fn with_agent_id<F, T>(id: AgentId, fut: F) -> Result<T>
where
    F: std::future::Future<Output = T>,
{
    let mut scope = task_local_context().current_scope()?;
    scope.agent_id = id;
    Ok(with_scope(scope, fut).await)
}
