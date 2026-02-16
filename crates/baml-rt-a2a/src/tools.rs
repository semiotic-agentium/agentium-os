//! A2A tool bundle for session-based interactions.

use crate::A2aRequestHandler;
use async_trait::async_trait;
use baml_rt_core::Result;
use baml_rt_tools::register_tool_metadata;
use baml_rt_tools::tools::ToolFunctionMetadata;
use baml_rt_tools::tools::ToolSessionContext;
use baml_rt_tools::tools::validate_open_input;
use baml_rt_tools::{
    BundleName, ToolBundle, ToolBundleMetadata, ToolCapability, ToolFailure, ToolHandler, ToolName,
    ToolSession, ToolSessionError, ToolStep, ToolTypeSpec, json_schema_value, ts_decl, ts_name,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use tokio::runtime::RuntimeFlavor;
use ts_rs::TS;

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
    handler: Arc<dyn A2aRequestHandler>,
}

impl A2aSessionBundle {
    pub fn new(handler: Arc<dyn A2aRequestHandler>) -> Self {
        Self { handler }
    }
}

impl ToolBundle for A2aSessionBundle {
    fn metadata(&self) -> ToolBundleMetadata {
        let name = BundleName::new("a2a".to_string()).expect("a2a bundle name must be valid");
        ToolBundleMetadata {
            name,
            description: "Agent-to-agent session interface".to_string(),
            config_schema: None,
            secret_requirements: Vec::new(),
        }
    }

    fn functions(&self) -> Vec<Arc<dyn ToolHandler>> {
        let metadata = a2a_session_metadata("a2a/session");
        vec![Arc::new(A2aSessionHandler {
            handler: self.handler.clone(),
            metadata,
        })]
    }
}

struct A2aSessionHandler {
    handler: Arc<dyn A2aRequestHandler>,
    metadata: ToolFunctionMetadata,
}

fn a2a_session_metadata(name: &str) -> ToolFunctionMetadata {
    let parsed = ToolName::parse(name).expect("a2a tool name must be valid");
    let class_name = ToolFunctionMetadata::derive_class_name(parsed.bundle(), parsed.local());
    ToolFunctionMetadata {
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
        access: None,
        session_plan_group: None,
        tags: vec!["a2a".to_string(), "session".to_string()],
        secret_requirements: Vec::new(),
        // ALL Rust tools are host tools - they must be declared in manifest.json
        origin: baml_rt_tools::ToolOrigin::Host,
    }
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
        // For A2A session, open_input should be empty object or null (unit type).
        validate_open_input::<()>(open_input)?;

        Ok(Box::new(A2aSession {
            ctx,
            handler: self.handler.clone(),
            queue: VecDeque::new(),
            closed: false,
        }))
    }
}

struct A2aSession {
    ctx: ToolSessionContext,
    handler: Arc<dyn A2aRequestHandler>,
    queue: VecDeque<Value>,
    closed: bool,
}

#[async_trait]
impl ToolSession for A2aSession {
    async fn send(&mut self, input: Value) -> std::result::Result<(), ToolSessionError> {
        if self.closed {
            return Err(ToolSessionError::Tool(ToolFailure::invalid_input(format!(
                "A2A session {session_id} is closed",
                session_id = self.ctx.session_id
            ))));
        }
        let parsed: A2aSessionInput = serde_json::from_value(input).map_err(|e| {
            ToolSessionError::Tool(ToolFailure::invalid_input(format!(
                "Invalid A2A input: {error}",
                error = e
            )))
        })?;
        let handle = tokio::runtime::Handle::current();
        let responses = match handle.runtime_flavor() {
            RuntimeFlavor::CurrentThread => {
                let handler = self.handler.clone();
                let request = parsed.request;
                let join = catch_unwind(AssertUnwindSafe(|| {
                    tokio::task::spawn_local(async move { handler.handle_a2a(request).await })
                }))
                .map_err(|_| {
                    ToolSessionError::Tool(ToolFailure::execution_failed(
                        "A2A session requires a LocalSet when running on a current-thread runtime"
                            .to_string(),
                    ))
                })?;
                join.await
                    .map_err(|e| {
                        ToolSessionError::Tool(ToolFailure::execution_failed(e.to_string()))
                    })?
                    .map_err(|e| {
                        ToolSessionError::Tool(ToolFailure::execution_failed(e.to_string()))
                    })?
            }
            _ => tokio::task::block_in_place(|| {
                handle.block_on(self.handler.handle_a2a(parsed.request))
            })
            .map_err(|e| ToolSessionError::Tool(ToolFailure::execution_failed(e.to_string())))?,
        };
        for response in responses {
            self.queue.push_back(response);
        }
        Ok(())
    }

    async fn next(&mut self) -> std::result::Result<ToolStep, ToolSessionError> {
        if let Some(response) = self.queue.pop_front() {
            let output = A2aSessionOutput { response };
            let value = serde_json::to_value(output).map_err(|e| {
                ToolSessionError::Tool(ToolFailure::execution_failed(format!(
                    "Invalid A2A output: {error}",
                    error = e
                )))
            })?;
            return Ok(ToolStep::Streaming { output: value });
        }
        Ok(ToolStep::Done { output: None })
    }

    async fn finish(&mut self) -> std::result::Result<(), ToolSessionError> {
        self.closed = true;
        Ok(())
    }

    async fn abort(
        &mut self,
        _reason: Option<String>,
    ) -> std::result::Result<(), ToolSessionError> {
        self.closed = true;
        Ok(())
    }
}
