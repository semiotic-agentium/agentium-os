//! Deterministic in-memory fake MCP server over JSON-RPC.
//!
//! Used by snapshot/importer/runtime tests so MCP support can be developed
//! without depending on real MCP servers or network access.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::Mutex,
};

pub const FAKE_PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FakeMcpTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub input_schema: Value,
    #[serde(default = "default_call_result")]
    pub call_result: Value,
}

fn default_call_result() -> Value {
    json!({
        "content": [{ "type": "text", "text": "ok" }],
        "isError": false
    })
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FakeMcpConfig {
    #[serde(default)]
    pub server_name: Option<String>,
    #[serde(default)]
    pub tools: Vec<FakeMcpTool>,
    /// When true, `tools/call` emits a `notifications/progress` before the result.
    #[serde(default)]
    pub progress_mode: bool,
    /// When true, the server sends `notifications/tools/list_changed` after the
    /// first successful `tools/call` and rewrites its tool list.
    #[serde(default)]
    pub drift_mode: bool,
    /// When true, `tools/call` returns a malformed JSON body to exercise
    /// protocol-error classification.
    #[serde(default)]
    pub malformed_response: bool,
}

impl FakeMcpConfig {
    pub fn with_tools(tools: Vec<FakeMcpTool>) -> Self {
        Self {
            tools,
            ..Default::default()
        }
    }
}

/// Observable state recorded by the fake server. Tests inspect it after the
/// server task exits or via the shared handle while running.
#[derive(Debug, Default)]
pub struct FakeMcpState {
    pub initialized: bool,
    pub requests: Vec<RequestRecord>,
    pub cancellations: Vec<Value>,
}

#[derive(Debug, Clone)]
pub struct RequestRecord {
    pub method: String,
    pub params: Value,
    pub id: Option<Value>,
}

pub type SharedFakeMcpState = Arc<Mutex<FakeMcpState>>;

pub fn new_state() -> SharedFakeMcpState {
    Arc::new(Mutex::new(FakeMcpState::default()))
}

/// Run the fake MCP server loop over the given async reader/writer. Returns
/// when the reader reports EOF or yields a fatal protocol error.
pub async fn run_fake_server<R, W>(
    config: FakeMcpConfig,
    state: SharedFakeMcpState,
    reader: R,
    mut writer: W,
) -> std::io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut tools = config.tools.clone();
    let mut drifted = false;
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    let mut call_count: u64 = 0;

    loop {
        line.clear();
        let read = reader.read_line(&mut line).await?;
        if read == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let message: Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(err) => {
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": {
                        "code": -32700,
                        "message": format!("parse error: {err}"),
                    }
                });
                write_message(&mut writer, &response).await?;
                continue;
            }
        };

        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let id = message.get("id").cloned();
        let params = message.get("params").cloned().unwrap_or(Value::Null);

        {
            let mut guard = state.lock().await;
            guard.requests.push(RequestRecord {
                method: method.clone(),
                params: params.clone(),
                id: id.clone(),
            });
        }

        match method.as_str() {
            "initialize" => {
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": FAKE_PROTOCOL_VERSION,
                        "capabilities": {
                            "tools": { "listChanged": true }
                        },
                        "serverInfo": {
                            "name": config.server_name.as_deref().unwrap_or("fake-mcp"),
                            "version": "0.1.0"
                        }
                    }
                });
                write_message(&mut writer, &response).await?;
            }
            "notifications/initialized" => {
                state.lock().await.initialized = true;
            }
            "tools/list" => {
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "tools": tools.iter().map(serialize_tool).collect::<Vec<_>>()
                    }
                });
                write_message(&mut writer, &response).await?;
            }
            "tools/call" => {
                call_count += 1;
                let tool_name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let tool = tools.iter().find(|tool| tool.name == tool_name).cloned();

                if config.progress_mode
                    && let Some(token) = params
                        .get("_meta")
                        .and_then(|meta| meta.get("progressToken"))
                        .cloned()
                {
                    let progress = json!({
                        "jsonrpc": "2.0",
                        "method": "notifications/progress",
                        "params": {
                            "progressToken": token,
                            "progress": 0.5,
                            "total": 1.0
                        }
                    });
                    write_message(&mut writer, &progress).await?;
                }

                if config.malformed_response {
                    writer.write_all(b"{ not json\n").await?;
                    writer.flush().await?;
                    continue;
                }

                let result = tool
                    .map(|t| t.call_result)
                    .unwrap_or_else(|| {
                        json!({
                            "content": [{ "type": "text", "text": format!("unknown tool {tool_name}") }],
                            "isError": true
                        })
                    });

                let response = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": result
                });
                write_message(&mut writer, &response).await?;

                if config.drift_mode && !drifted && call_count == 1 {
                    drifted = true;
                    for tool in tools.iter_mut() {
                        tool.description = Some(format!(
                            "{} (drifted)",
                            tool.description.clone().unwrap_or_default()
                        ));
                    }
                    let notif = json!({
                        "jsonrpc": "2.0",
                        "method": "notifications/tools/list_changed",
                        "params": {}
                    });
                    write_message(&mut writer, &notif).await?;
                }
            }
            "notifications/cancelled" => {
                state.lock().await.cancellations.push(params);
            }
            "shutdown" => {
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": null
                });
                write_message(&mut writer, &response).await?;
            }
            "exit" => break,
            other => {
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32601,
                        "message": format!("method not found: {other}")
                    }
                });
                write_message(&mut writer, &response).await?;
            }
        }
    }

    Ok(())
}

fn serialize_tool(tool: &FakeMcpTool) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("name".into(), Value::String(tool.name.clone()));
    if let Some(desc) = tool.description.clone() {
        obj.insert("description".into(), Value::String(desc));
    }
    obj.insert("inputSchema".into(), tool.input_schema.clone());
    Value::Object(obj)
}

async fn write_message<W>(writer: &mut W, value: &Value) -> std::io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut payload = serde_json::to_vec(value)?;
    payload.push(b'\n');
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    use super::*;

    fn sample_tools() -> Vec<FakeMcpTool> {
        vec![
            FakeMcpTool {
                name: "search".into(),
                description: Some("search docs".into()),
                input_schema: json!({ "type": "object", "properties": { "q": { "type": "string" } } }),
                call_result: json!({
                    "content": [{ "type": "text", "text": "found 2 results" }],
                    "isError": false
                }),
            },
            FakeMcpTool {
                name: "query".into(),
                description: Some("query db".into()),
                input_schema: json!({ "type": "object", "properties": {} }),
                call_result: json!({
                    "content": [{ "type": "text", "text": "ok" }],
                    "isError": false
                }),
            },
        ]
    }

    async fn run_scenario<C, Fut>(config: FakeMcpConfig, client: C) -> (FakeMcpState, Fut::Output)
    where
        C: FnOnce(tokio::io::DuplexStream) -> Fut,
        Fut: std::future::Future,
    {
        let state = new_state();
        let (client_io, server_io) = tokio::io::duplex(8192);
        let (server_read, server_write) = tokio::io::split(server_io);
        let server_state = state.clone();
        let server = tokio::spawn(async move {
            run_fake_server(config, server_state, server_read, server_write)
                .await
                .expect("server loop");
        });
        let output = client(client_io).await;
        let _ = server.await;
        let guard = state.lock().await;
        let final_state = FakeMcpState {
            initialized: guard.initialized,
            requests: guard.requests.clone(),
            cancellations: guard.cancellations.clone(),
        };
        (final_state, output)
    }

    async fn read_one_message<R: tokio::io::AsyncRead + Unpin>(reader: &mut BufReader<R>) -> Value {
        let mut line = String::new();
        reader.read_line(&mut line).await.expect("read line");
        serde_json::from_str(line.trim()).expect("valid json line")
    }

    #[tokio::test]
    async fn initialize_and_list_two_tools() {
        let config = FakeMcpConfig::with_tools(sample_tools());
        let (final_state, _) = run_scenario(config, |io| async move {
            let (read, mut write) = tokio::io::split(io);
            let mut read = BufReader::new(read);

            write
                .write_all(
                    b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n",
                )
                .await
                .unwrap();
            let init = read_one_message(&mut read).await;
            assert_eq!(init["result"]["protocolVersion"], FAKE_PROTOCOL_VERSION);

            write
                .write_all(
                    b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\",\"params\":{}}\n",
                )
                .await
                .unwrap();

            write
                .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n")
                .await
                .unwrap();
            let listed = read_one_message(&mut read).await;
            let tools = listed["result"]["tools"].as_array().expect("tools array");
            assert_eq!(tools.len(), 2);
            assert_eq!(tools[0]["name"], "search");
            assert_eq!(tools[1]["name"], "query");

            drop(write);
            drop(read);
        })
        .await;

        assert!(final_state.initialized);
        let methods: Vec<&str> = final_state
            .requests
            .iter()
            .map(|r| r.method.as_str())
            .collect();
        assert_eq!(
            methods,
            vec!["initialize", "notifications/initialized", "tools/list"]
        );
    }

    #[tokio::test]
    async fn tools_call_returns_deterministic_result() {
        let config = FakeMcpConfig::with_tools(sample_tools());
        let (_, _) = run_scenario(config, |io| async move {
            let (read, mut write) = tokio::io::split(io);
            let mut read = BufReader::new(read);
            write
                .write_all(
                    b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"search\",\"arguments\":{}}}\n",
                )
                .await
                .unwrap();
            let resp = read_one_message(&mut read).await;
            assert_eq!(
                resp["result"]["content"][0]["text"],
                "found 2 results"
            );
            drop(write);
            drop(read);
        })
        .await;
    }

    #[tokio::test]
    async fn cancellation_notification_is_recorded() {
        let config = FakeMcpConfig::with_tools(sample_tools());
        let (final_state, _) = run_scenario(config, |io| async move {
            let (_read, mut write) = tokio::io::split(io);
            write
                .write_all(
                    b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/cancelled\",\"params\":{\"requestId\":7,\"reason\":\"client\"}}\n",
                )
                .await
                .unwrap();
            drop(write);
        })
        .await;

        assert_eq!(final_state.cancellations.len(), 1);
        assert_eq!(final_state.cancellations[0]["requestId"], 7);
        assert_eq!(final_state.cancellations[0]["reason"], "client");
    }

    #[tokio::test]
    async fn malformed_response_mode_emits_invalid_json() {
        let mut config = FakeMcpConfig::with_tools(sample_tools());
        config.malformed_response = true;
        let (_, body) = run_scenario(config, |io| async move {
            let (read, mut write) = tokio::io::split(io);
            let mut read = BufReader::new(read);
            write
                .write_all(
                    b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"search\",\"arguments\":{}}}\n",
                )
                .await
                .unwrap();
            let mut line = String::new();
            read.read_line(&mut line).await.unwrap();
            drop(write);
            line
        })
        .await;
        assert!(serde_json::from_str::<Value>(body.trim()).is_err());
    }

    #[tokio::test]
    async fn drift_mode_emits_list_changed_after_first_call() {
        let mut config = FakeMcpConfig::with_tools(sample_tools());
        config.drift_mode = true;
        let (_, _) = run_scenario(config, |io| async move {
            let (read, mut write) = tokio::io::split(io);
            let mut read = BufReader::new(read);
            write
                .write_all(
                    b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"search\",\"arguments\":{}}}\n",
                )
                .await
                .unwrap();
            let resp = read_one_message(&mut read).await;
            assert!(resp.get("result").is_some());
            let notif = read_one_message(&mut read).await;
            assert_eq!(notif["method"], "notifications/tools/list_changed");
            drop(write);
        })
        .await;
    }

    #[tokio::test]
    async fn progress_mode_emits_progress_notification() {
        let mut config = FakeMcpConfig::with_tools(sample_tools());
        config.progress_mode = true;
        let (_, _) = run_scenario(config, |io| async move {
            let (read, mut write) = tokio::io::split(io);
            let mut read = BufReader::new(read);
            write
                .write_all(
                    b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"search\",\"arguments\":{},\"_meta\":{\"progressToken\":\"p1\"}}}\n",
                )
                .await
                .unwrap();
            let first = read_one_message(&mut read).await;
            assert_eq!(first["method"], "notifications/progress");
            assert_eq!(first["params"]["progressToken"], "p1");
            let second = read_one_message(&mut read).await;
            assert!(second.get("result").is_some());
            drop(write);
        })
        .await;
    }

    #[tokio::test]
    async fn unknown_method_returns_error_response() {
        let config = FakeMcpConfig::with_tools(sample_tools());
        let (_, _) = run_scenario(config, |io| async move {
            let (read, mut write) = tokio::io::split(io);
            let mut read = BufReader::new(read);
            write
                .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":99,\"method\":\"resources/list\"}\n")
                .await
                .unwrap();
            let resp = read_one_message(&mut read).await;
            assert_eq!(resp["error"]["code"], -32601);
            drop(write);
        })
        .await;
    }
}
