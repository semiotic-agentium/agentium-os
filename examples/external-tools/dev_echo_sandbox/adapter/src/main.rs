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
            tool_name: "dev/echo".to_string(),
            supported_methods: SUPPORTED_METHODS.iter().map(|s| (*s).to_string()).collect(),
            max_payload_bytes: None,
            schema_hash: None,
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

        let reply = params
            .input
            .get("message")
            .and_then(Value::as_str)
            .ok_or_else(|| AdapterError::InvalidArgument("message must be a string".to_string()))?
            .to_string();

        Ok(ToolInvokeResult {
            output: json!({ "reply": reply }),
            done: true,
        })
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(err) = run_adapter(EchoTool).await {
        eprintln!("dev-echo-sandbox-adapter exited with error: {err}");
        std::process::exit(1);
    }
}
