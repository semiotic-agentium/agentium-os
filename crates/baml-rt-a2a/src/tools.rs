//! A2A tool bundle for session-based interactions.
//!
//! Channel-based design: send() only enqueues; dispatcher and runtime worker are tokio tasks.
//! A2A work is dispatched via handler.run_handle_a2a(), which uses bridge post_to_worker_void.
//! No threads for orchestration.
//! See `docs/A2A_SESSION_CHANNEL_DESIGN.md` and `docs/INVARIANTS_AND_LIVENESS.md`.

use crate::A2aRequestHandler;
use crate::session_channel::{
    DispatcherMsg, RuntimeWorkerMsg, SessionCmd, run_dispatcher, run_runtime_worker,
};
use async_trait::async_trait;
use baml_rt_core::context::RuntimeScope;
use baml_rt_core::{BamlRtError, Result};
use baml_rt_tools::register_tool_metadata;
use baml_rt_tools::tools::ToolFunctionMetadata;
use baml_rt_tools::tools::ToolSessionContext;
use baml_rt_tools::{
    BundleName, ToolBundle, ToolBundleMetadata, ToolCapability, ToolFailure, ToolHandler, ToolName,
    ToolSession, ToolSessionError, ToolSessionId, ToolStep, ToolTypeSpec, json_schema_value,
    ts_decl, ts_name,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::mpsc;
use ts_rs::TS;

/// Session phase for host-side FSM; mirrors JS wrapper terminal-state checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionPhase {
    Open,
    Running,
    Closing,
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct A2aSessionInput {
    #[ts(type = "any")]
    pub request: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct A2aSessionOutput {
    #[ts(type = "any")]
    pub response: Value,
}

pub struct A2aSessionBundle {
    dispatcher: Arc<A2aRequestDispatcher>,
    bundle_metadata: ToolBundleMetadata,
    session_tool_metadata: ToolFunctionMetadata,
}

impl A2aSessionBundle {
    /// Creates the bundle: spawns the dispatcher and runtime worker as tokio tasks.
    /// Work runs through handler.run_handle_a2a() and bridge post_to_worker_void.
    /// Validates protocol constants ("a2a" bundle name, "a2a/session" tool name) at construction.
    pub fn new(handler: Arc<dyn A2aRequestHandler>) -> Result<Self> {
        let name = BundleName::new("a2a".to_string())
            .map_err(|_| BamlRtError::InvalidArgument("Invalid a2a bundle name".into()))?;
        let bundle_metadata = ToolBundleMetadata {
            name,
            description: "Agent-to-agent session interface".to_string(),
            config_schema: None,
            secret_requirements: Vec::new(),
        };
        let session_tool_metadata = a2a_session_metadata_result("a2a/session")?;
        Ok(Self {
            dispatcher: Arc::new(A2aRequestDispatcher::new(handler)),
            bundle_metadata,
            session_tool_metadata,
        })
    }
}

impl ToolBundle for A2aSessionBundle {
    fn metadata(&self) -> ToolBundleMetadata {
        self.bundle_metadata.clone()
    }

    fn functions(&self) -> Vec<Arc<dyn ToolHandler>> {
        let metadata = self.session_tool_metadata.clone();
        vec![Arc::new(A2aSessionHandler {
            dispatcher: self.dispatcher.clone(),
            metadata,
        })]
    }
}

struct A2aSessionHandler {
    dispatcher: Arc<A2aRequestDispatcher>,
    metadata: ToolFunctionMetadata,
}

/// Dispatcher handle: sends Register/Cmd to the dispatcher task (MT). Scope is in the message.
struct A2aRequestDispatcher {
    tx: mpsc::UnboundedSender<DispatcherMsg>,
}

impl A2aRequestDispatcher {
    fn new(handler: Arc<dyn A2aRequestHandler>) -> Self {
        let (worker_tx, worker_rx) = mpsc::unbounded_channel::<RuntimeWorkerMsg>();
        let (dispatcher_tx, dispatcher_rx) = mpsc::unbounded_channel::<DispatcherMsg>();

        let parent = tracing::Span::current();
        tokio::spawn(async move {
            let _guard = parent.enter();
            run_dispatcher(dispatcher_rx, worker_tx).await
        });
        let parent_worker = tracing::Span::current();
        tokio::spawn(async move {
            let _guard = parent_worker.enter();
            run_runtime_worker(handler, worker_rx).await
        });

        Self { tx: dispatcher_tx }
    }

    fn register(
        &self,
        session_id: ToolSessionId,
        scope: RuntimeScope,
        response_tx: mpsc::UnboundedSender<Value>,
    ) -> std::result::Result<(), ToolSessionError> {
        self.tx
            .send(DispatcherMsg::Register {
                session_id,
                scope,
                response_tx,
            })
            .map_err(|_| {
                ToolSessionError::Tool(ToolFailure::execution_failed(
                    "A2A session dispatcher channel closed".to_string(),
                ))
            })
    }

    fn send_cmd(
        &self,
        session_id: ToolSessionId,
        cmd: SessionCmd,
    ) -> std::result::Result<(), ToolSessionError> {
        self.tx
            .send(DispatcherMsg::Cmd { session_id, cmd })
            .map_err(|_| {
                ToolSessionError::Tool(ToolFailure::execution_failed(
                    "A2A session dispatcher channel closed".to_string(),
                ))
            })
    }
}

fn a2a_session_metadata_result(name: &str) -> Result<ToolFunctionMetadata> {
    let parsed = ToolName::parse(name)?;
    let class_name = ToolFunctionMetadata::derive_class_name(parsed.bundle(), parsed.local());
    Ok(ToolFunctionMetadata {
        name: parsed.clone(),
        class_name,
        description: "Bidirectional A2A session call".to_string(),
        open_input_schema: json_schema_value::<()>(),
        input_schema: json_schema_value::<A2aSessionInput>(),
        output_schema: json_schema_value::<A2aSessionOutput>(),
        open_input_type: ToolTypeSpec {
            name: ts_name::<()>(),
            ts_decl: ts_decl::<()>(),
        },
        input_type: ToolTypeSpec {
            name: ts_name::<A2aSessionInput>(),
            ts_decl: ts_decl::<A2aSessionInput>(),
        },
        output_type: ToolTypeSpec {
            name: ts_name::<A2aSessionOutput>(),
            ts_decl: ts_decl::<A2aSessionOutput>(),
        },
        baml_decl: None,
        extra_ts_decls: Vec::new(),
        tags: vec!["a2a".to_string(), "session".to_string()],
        secret_requirements: Vec::new(),
        access: None,
        // ALL Rust tools are host tools - they must be declared in manifest.json
        origin: baml_rt_tools::ToolOrigin::Host,
    })
}

fn a2a_session_metadata(name: &str) -> ToolFunctionMetadata {
    a2a_session_metadata_result(name).expect("a2a/session is a validated protocol constant")
}

fn a2a_session_metadata_qualified() -> ToolFunctionMetadata {
    a2a_session_metadata("a2a/session")
}

register_tool_metadata!(a2a_session_metadata_qualified);

#[async_trait]
impl ToolHandler for A2aSessionHandler {
    fn metadata(&self) -> &ToolFunctionMetadata {
        &self.metadata
    }

    fn capability(&self) -> ToolCapability {
        ToolCapability::Streaming
    }

    async fn open_session(
        &self,
        ctx: ToolSessionContext,
        open_input: Value,
    ) -> Result<Box<dyn ToolSession>> {
        // Scope must be in open_input (channel-based design: context in messages).
        let scope_value = open_input.get("scope").cloned().ok_or_else(|| {
            baml_rt_core::BamlRtError::InvalidArgument(
                "A2A session requires scope in open_input; use explicit-scope open_tool_session API".into(),
            )
        })?;
        let scope: RuntimeScope = serde_json::from_value(scope_value)
            .map_err(|e| baml_rt_core::BamlRtError::InvalidOpenInput { source: e })?;

        let (response_tx, response_rx) = mpsc::unbounded_channel::<Value>();
        self.dispatcher
            .register(ctx.session_id.clone(), scope, response_tx)
            .map_err(|e| baml_rt_core::BamlRtError::ToolExecution(format!("{:?}", e)))?;

        Ok(Box::new(A2aSession {
            ctx,
            dispatcher: self.dispatcher.clone(),
            response_rx,
            phase: SessionPhase::Open,
        }))
    }
}

struct A2aSession {
    ctx: ToolSessionContext,
    dispatcher: Arc<A2aRequestDispatcher>,
    response_rx: mpsc::UnboundedReceiver<Value>,
    phase: SessionPhase,
}

#[async_trait]
impl ToolSession for A2aSession {
    /// Enqueues request only; returns immediately (sub-5ms). Worker on LocalSet runs handle_a2a.
    async fn send(&mut self, input: Value) -> std::result::Result<(), ToolSessionError> {
        if self.phase == SessionPhase::Closing || self.phase == SessionPhase::Closed {
            return Err(ToolSessionError::Tool(ToolFailure::invalid_input(format!(
                "A2A session {} cannot send after terminal phase {:?}",
                self.ctx.session_id, self.phase
            ))));
        }
        let parsed: A2aSessionInput = serde_json::from_value(input).map_err(|e| {
            ToolSessionError::Tool(ToolFailure::invalid_input(format!(
                "Invalid A2A input: {}",
                e
            )))
        })?;
        self.dispatcher.send_cmd(
            self.ctx.session_id.clone(),
            SessionCmd::Send(parsed.request),
        )?;
        self.phase = SessionPhase::Running;
        Ok(())
    }

    /// Drains per-session response channel; Done when channel closed (worker sent Finish/Abort).
    async fn next(&mut self) -> std::result::Result<ToolStep, ToolSessionError> {
        match self.response_rx.recv().await {
            Some(response) => {
                let output = A2aSessionOutput { response };
                let value = serde_json::to_value(output).map_err(|e| {
                    ToolSessionError::Tool(ToolFailure::execution_failed(format!(
                        "Invalid A2A output: {}",
                        e
                    )))
                })?;
                Ok(ToolStep::Streaming { output: value })
            }
            None => Ok(ToolStep::Done { output: None }),
        }
    }

    async fn finish(&mut self) -> std::result::Result<(), ToolSessionError> {
        if self.phase == SessionPhase::Closed {
            return Ok(());
        }
        self.dispatcher
            .send_cmd(self.ctx.session_id.clone(), SessionCmd::Finish)?;
        self.phase = SessionPhase::Closed;
        Ok(())
    }

    async fn abort(&mut self, reason: Option<String>) -> std::result::Result<(), ToolSessionError> {
        if self.phase == SessionPhase::Closed {
            return Ok(());
        }
        self.dispatcher
            .send_cmd(self.ctx.session_id.clone(), SessionCmd::Abort(reason))?;
        self.phase = SessionPhase::Closed;
        Ok(())
    }
}
