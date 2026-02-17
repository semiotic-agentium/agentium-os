//! In-memory A2A test client.

use async_trait::async_trait;
use baml_rt::tools::BamlTool;
use baml_rt::{A2aRequestHandler, Result};
use baml_rt_tools::bundles::Support;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio::task;
use ts_rs::TS;

#[derive(Clone)]
pub struct A2aInMemoryClient {
    target: Arc<dyn A2aRequestHandler>,
}

impl A2aInMemoryClient {
    pub fn new(target: Arc<dyn A2aRequestHandler>) -> Self {
        Self { target }
    }

    pub async fn send(&self, request: Value) -> Result<Vec<Value>> {
        Ok(baml_rt_core::collect_a2a_stream(self.target.handle_a2a_stream(request).await?).await)
    }
}

pub struct A2aRelayTool {
    client: A2aInMemoryClient,
}

impl A2aRelayTool {
    pub fn new(client: A2aInMemoryClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl BamlTool for A2aRelayTool {
    type Bundle = Support;
    const LOCAL_NAME: &'static str = "a2a_relay";
    type OpenInput = ();
    type Input = A2aRelayInput;
    type Output = A2aRelayOutput;

    fn description(&self) -> &'static str {
        "Relays an A2A request to another in-memory agent."
    }

    async fn execute(&self, args: Self::Input) -> Result<Self::Output> {
        let handle = tokio::runtime::Handle::current();
        let responses = task::block_in_place(|| handle.block_on(self.client.send(args.request)))?;
        Ok(A2aRelayOutput { responses })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct A2aRelayInput {
    #[ts(type = "any")]
    request: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, TS)]
#[ts(export)]
pub struct A2aRelayOutput {
    #[ts(type = "any[]")]
    responses: Vec<Value>,
}
