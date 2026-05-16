//! Minimal stdio JSON-RPC client used only by the import path.
//!
//! Speaks just enough of MCP to run a discovery handshake:
//! `initialize` -> `notifications/initialized` -> `tools/list`. The runtime
//! tool-call path (PR 4) will use `rmcp` for the full client surface; this
//! module exists to keep PR 2 free of an unfamiliar transport dependency
//! and to stay laser-focused on import.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{ChildStdin, ChildStdout},
    time::error::Elapsed,
};

use crate::wire::write_json_line;

/// Protocol version advertised during `initialize`. Pinned per current
/// platform decision; bumping this is an explicit design step.
pub const CLIENT_PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Debug, Error)]
pub enum McpRpcError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("server closed connection before responding")]
    Eof,
    #[error("server returned malformed JSON-RPC line: {0}")]
    Malformed(String),
    #[error("jsonrpc error {code}: {message}")]
    Server { code: i64, message: String },
    #[error("timed out waiting for server response")]
    Timeout,
    #[error("response missing `result` field")]
    MissingResult,
}

impl From<Elapsed> for McpRpcError {
    fn from(_: Elapsed) -> Self {
        McpRpcError::Timeout
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    #[serde(default)]
    pub capabilities: Value,
    #[serde(rename = "serverInfo", default)]
    pub server_info: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "inputSchema", default)]
    pub input_schema: Value,
    #[serde(default)]
    pub annotations: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsList {
    pub tools: Vec<ToolDescriptor>,
}

pub struct McpStdioClient {
    next_id: u64,
    writer: ChildStdin,
    reader: BufReader<ChildStdout>,
    line: String,
}

impl McpStdioClient {
    pub fn new(stdin: ChildStdin, stdout: ChildStdout) -> Self {
        Self {
            next_id: 1,
            writer: stdin,
            reader: BufReader::new(stdout),
            line: String::new(),
        }
    }

    pub async fn initialize(&mut self, timeout: Duration) -> Result<InitializeResult, McpRpcError> {
        let params = json!({
            "protocolVersion": CLIENT_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "baml-rt-importer", "version": env!("CARGO_PKG_VERSION") }
        });
        let result = self.request("initialize", params, timeout).await?;
        let parsed: InitializeResult = serde_json::from_value(result)
            .map_err(|err| McpRpcError::Malformed(err.to_string()))?;
        self.notify("notifications/initialized", json!({})).await?;
        Ok(parsed)
    }

    pub async fn list_tools(&mut self, timeout: Duration) -> Result<ToolsList, McpRpcError> {
        let result = self.request("tools/list", json!({}), timeout).await?;
        serde_json::from_value(result).map_err(|err| McpRpcError::Malformed(err.to_string()))
    }

    async fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, McpRpcError> {
        let id = self.next_id;
        self.next_id += 1;
        let envelope = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.write_message(&envelope).await?;

        loop {
            let response = tokio::time::timeout(timeout, self.read_message()).await??;
            let response_id = response.get("id").cloned().unwrap_or(Value::Null);
            if response_id != Value::Number(id.into()) {
                // Notification or out-of-band message; drop and continue.
                continue;
            }
            if let Some(error) = response.get("error") {
                let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("(no message)")
                    .to_string();
                return Err(McpRpcError::Server { code, message });
            }
            return response
                .get("result")
                .cloned()
                .ok_or(McpRpcError::MissingResult);
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), McpRpcError> {
        let envelope = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write_message(&envelope).await
    }

    async fn write_message(&mut self, value: &Value) -> Result<(), McpRpcError> {
        write_json_line(&mut self.writer, value).await?;
        Ok(())
    }

    async fn read_message(&mut self) -> Result<Value, McpRpcError> {
        loop {
            self.line.clear();
            let read = self.reader.read_line(&mut self.line).await?;
            if read == 0 {
                return Err(McpRpcError::Eof);
            }
            let trimmed = self.line.trim();
            if trimmed.is_empty() {
                continue;
            }
            return serde_json::from_str::<Value>(trimmed)
                .map_err(|err| McpRpcError::Malformed(err.to_string()));
        }
    }
}
