// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! In-memory A2A test client.

use std::sync::Arc;

use async_trait::async_trait;
use baml_derive::BamlType;
use baml_rt::{A2aRequestHandler, Result, tools::BamlTool};
use baml_rt_core::A2aJsChatHost;
use baml_rt_tools::{OpaqueJson, bundles::Support};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::task;

#[derive(Clone)]
pub struct A2aInMemoryClient {
    target: Arc<dyn A2aRequestHandler>,
}

impl A2aInMemoryClient {
    pub fn new(target: Arc<dyn A2aRequestHandler>) -> Self {
        Self { target }
    }

    /// Prefer for tests that require full HTTP parity: requires a handler that implements
    /// [`A2aJsChatHost`] (e.g. [`baml_rt::A2aAgent`]). Registration-time verification of the
    /// QuickJS surface runs inside `register_baml_functions` on the agent build path.
    pub fn new_for_chat_parity(host: Arc<dyn A2aJsChatHost>) -> Self {
        let target: Arc<dyn A2aRequestHandler> = host;
        Self { target }
    }

    pub async fn send(&self, request: Value) -> Result<Vec<Value>> {
        let stream = self
            .target
            .handle_a2a_stream(baml_rt_core::A2aWireRequest::from(request))
            .await?;
        let chunks = baml_rt_core::collect_a2a_stream_one_shot(stream).await;
        Ok(chunks
            .into_iter()
            .map(baml_rt_core::A2aStreamChunk::into_inner)
            .collect())
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
        let request = args.request.into_inner();
        let responses = task::block_in_place(|| handle.block_on(self.client.send(request)))?;
        Ok(A2aRelayOutput {
            responses: responses.into_iter().map(OpaqueJson::from).collect(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
pub struct A2aRelayInput {
    request: OpaqueJson,
}
impl baml_rt_tools::DescribeAction for A2aRelayInput {
    fn describe(&self) -> String {
        "relaying A2A request".to_string()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, BamlType)]
pub struct A2aRelayOutput {
    responses: Vec<OpaqueJson>,
}
