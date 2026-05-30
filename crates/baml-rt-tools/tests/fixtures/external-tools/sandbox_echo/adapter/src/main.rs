// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Reference echo adapter fixture.
//!
//! Runs the `baml-sandbox-adapter` SDK with a minimal tool that echoes
//! the caller-supplied `message` back as `reply`. Used by:
//!
//! - X4.2 slice-5 stdio E2E tests (happy path, stdout purity, panic
//!   keepalive, malformed frames, version mismatch, shutdown flush).
//! - X4.3 distroless image build.
//! - X5 KVM-gated live-microVM E2E.
//!
//! Deliberate branches exposed for the E2E surface:
//! - `input.panic == true` → panics, so the suite can assert that the
//!   adapter converts the panic into an error frame and keeps serving.
//! - `input.pollute == true` → emits a raw `println!` before replying,
//!   so the suite can assert the stdout-fd swap kept the wire clean.

use async_trait::async_trait;
use baml_sandbox_adapter::{AdapterError, SandboxTool, run_adapter};
use baml_sandbox_protocol::{
    PROTOCOL_VERSION, SUPPORTED_METHODS, ToolDescribeResult, ToolInvokeParams, ToolInvokeResult,
};
use serde_json::{Value, json};

struct EchoTool;

#[async_trait]
impl SandboxTool for EchoTool {
    fn describe(&self) -> ToolDescribeResult {
        ToolDescribeResult {
            protocol_version: PROTOCOL_VERSION.to_string(),
            tool_name: "sandbox-echo".to_string(),
            supported_methods: SUPPORTED_METHODS.iter().map(|s| (*s).to_string()).collect(),
            max_payload_bytes: None,
            schema_digest: None,
            capabilities: None,
        }
    }

    async fn invoke(&self, params: ToolInvokeParams) -> Result<ToolInvokeResult, AdapterError> {
        if params.input.get("panic") == Some(&Value::Bool(true)) {
            panic!("echo received panic:true");
        }
        if params.input.get("pollute") == Some(&Value::Bool(true)) {
            println!("echo pollution attempt");
        }
        let reply = params.input.get("message").cloned().unwrap_or(Value::Null);
        Ok(ToolInvokeResult {
            output: json!({ "reply": reply }),
            done: true,
        })
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(err) = run_adapter(EchoTool).await {
        eprintln!("sandbox-echo-adapter exited with error: {err}");
        std::process::exit(1);
    }
}
