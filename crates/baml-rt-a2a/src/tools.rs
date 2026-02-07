//! A2A tool bundle for session-based interactions.

use crate::A2aRequestHandler;
use async_trait::async_trait;
use baml_rt_core::Result;
use baml_rt_tools::tools::ToolFunctionMetadata;
use baml_rt_tools::{
    json_schema_value, ts_decl, ts_name, BundleName, ToolBundle, ToolBundleMetadata,
    ToolCapability, ToolFailure, ToolHandler, ToolName, ToolSession,
    ToolSessionError, ToolStep, ToolTypeSpec,
};
use baml_rt_tools::tools::ToolSessionContext;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::Arc;
use ts_rs::TS;
use baml_rt_tools::register_tool_metadata;

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
        let name = BundleName::new("a2a".to_string())
            .expect("a2a bundle name must be valid");
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

    async fn open_session(&self, ctx: ToolSessionContext) -> Result<Box<dyn ToolSession>> {
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
                "A2A session {} is closed",
                self.ctx.session_id
            ))));
        }
        let parsed: A2aSessionInput = serde_json::from_value(input)
            .map_err(|e| ToolSessionError::Tool(ToolFailure::invalid_input(format!("Invalid A2A input: {}", e))))?;
        let handle = tokio::runtime::Handle::current();
        let responses = tokio::task::block_in_place(|| handle.block_on(self.handler.handle_a2a(parsed.request)))
            .map_err(|e| ToolSessionError::Tool(ToolFailure::execution_failed(e.to_string())))?;
        for response in responses {
            self.queue.push_back(response);
        }
        Ok(())
    }

    async fn next(&mut self) -> std::result::Result<ToolStep, ToolSessionError> {
        if let Some(response) = self.queue.pop_front() {
            let output = A2aSessionOutput { response };
            let value = serde_json::to_value(output)
                .map_err(|e| ToolSessionError::Tool(ToolFailure::execution_failed(format!("Invalid A2A output: {}", e))))?;
            return Ok(ToolStep::Streaming { output: value });
        }
        Ok(ToolStep::Done { output: None })
    }

    async fn finish(&mut self) -> std::result::Result<(), ToolSessionError> {
        self.closed = true;
        Ok(())
    }

    async fn abort(&mut self, _reason: Option<String>) -> std::result::Result<(), ToolSessionError> {
        self.closed = true;
        Ok(())
    }
}
