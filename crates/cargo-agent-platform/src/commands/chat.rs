//! `chat` subcommand — interactive terminal chat with a deployed agent.

use std::{
    fmt,
    io::{self, Write},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use console::style;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize)]
struct AgentDiscoveryEntry {
    agent_card: AgentCard,
}

#[derive(Debug, Deserialize)]
struct AgentCard {
    name: String,
    agent_package: String,
    agent_instance_id: String,
}

#[derive(Debug, Serialize)]
struct JsonRpcRequest<'a> {
    jsonrpc: &'static str,
    method: &'static str,
    id: &'a str,
    params: SendMessageParams<'a>,
}

#[derive(Debug, Serialize)]
struct SendMessageParams<'a> {
    message: Message<'a>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Message<'a> {
    message_id: &'a str,
    context_id: &'a str,
    role: &'static str,
    parts: Vec<Part<'a>>,
}

#[derive(Debug, Serialize)]
struct Part<'a> {
    text: &'a str,
}

#[derive(Debug)]
struct ChatTurnResult {
    text: String,
    elapsed_ms: u128,
}

#[derive(Debug)]
struct ChatCommandError {
    tag: &'static str,
    message: String,
}

impl fmt::Display for ChatCommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ChatCommandError {}

fn tagged_error(tag: &'static str, message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(ChatCommandError {
        tag,
        message: message.into(),
    })
}

fn extract_chunk_or_result(value: &Value) -> Option<&Value> {
    let result = value.get("result")?;
    result.get("chunk").or(Some(result))
}

fn extract_text_from_chunk(chunk: &Value) -> Option<String> {
    fn parts_text(message: &Value) -> Option<String> {
        let parts = message.get("parts")?.as_array()?;
        let texts: Vec<&str> = parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect();
        if texts.is_empty() {
            None
        } else {
            Some(texts.join("\n"))
        }
    }

    if let Some(message) = chunk.get("message")
        && let Some(text) = parts_text(message)
    {
        return Some(text);
    }

    if let Some(text) = chunk
        .get("task")
        .and_then(|t| t.get("status"))
        .and_then(|s| s.get("message"))
        .and_then(parts_text)
    {
        return Some(text);
    }

    if let Some(text) = chunk
        .get("statusUpdate")
        .and_then(|s| s.get("status"))
        .and_then(|s| s.get("message"))
        .and_then(parts_text)
    {
        return Some(text);
    }

    if let Some(task) = chunk.get("task")
        && let Some(history) = task.get("history").and_then(Value::as_array)
    {
        for msg in history.iter().rev() {
            let role = msg.get("role").and_then(Value::as_str);
            if matches!(role, Some("ROLE_AGENT") | Some("agent") | Some("assistant"))
                && let Some(text) = parts_text(msg)
            {
                return Some(text);
            }
        }
    }

    None
}

fn is_non_user_facing_text(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with("Calling model:")
        || trimmed.starts_with("Invoking tool:")
        || trimmed.starts_with("ClickUp session produced:")
}

fn extract_rpc_error(value: &Value) -> Option<String> {
    let error = value.get("error")?;
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown rpc error");
    let code = error.get("code").and_then(Value::as_i64);
    let details = error
        .get("data")
        .and_then(|d| d.get("details"))
        .and_then(Value::as_str);

    let mut parts = Vec::new();
    parts.push(message.to_string());
    if let Some(c) = code {
        parts.push(format!("code={c}"));
    }
    if let Some(d) = details
        && !d.trim().is_empty()
    {
        parts.push(format!("details={d}"));
    }
    Some(parts.join(" | "))
}

fn terminal_state_from_chunk<'a>(chunk: &'a Value) -> Option<&'a str> {
    chunk
        .get("task")
        .and_then(|t| t.get("status"))
        .and_then(|s| s.get("state"))
        .and_then(Value::as_str)
        .or_else(|| {
            chunk
                .get("statusUpdate")
                .and_then(|s| s.get("status"))
                .and_then(|s| s.get("state"))
                .and_then(Value::as_str)
        })
}

fn terminal_message_from_chunk(chunk: &Value) -> Option<String> {
    chunk
        .get("task")
        .and_then(|t| t.get("status"))
        .and_then(|s| s.get("message"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            chunk
                .get("statusUpdate")
                .and_then(|s| s.get("status"))
                .and_then(|s| s.get("message"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
}

fn is_terminal(value: &Value) -> bool {
    if value
        .get("result")
        .and_then(|r| r.get("final"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return true;
    }
    matches!(
        extract_chunk_or_result(value).and_then(terminal_state_from_chunk),
        Some(
            "TASK_STATE_COMPLETED"
                | "TASK_STATE_FAILED"
                | "TASK_STATE_CANCELED"
                | "TASK_STATE_REJECTED"
                | "completed"
                | "failed"
                | "canceled"
                | "rejected"
        )
    )
}

fn is_failure_state(state: &str) -> bool {
    matches!(
        state,
        "TASK_STATE_FAILED"
            | "TASK_STATE_CANCELED"
            | "TASK_STATE_REJECTED"
            | "failed"
            | "canceled"
            | "rejected"
    )
}

fn parse_sse_data_line(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.is_empty() || !trimmed.starts_with("data:") {
        return None;
    }
    let data = trimmed.strip_prefix("data:").unwrap_or(trimmed).trim();
    if data.is_empty() { None } else { Some(data) }
}

fn format_time_line(ms: u128) -> String {
    if ms < 1_000 {
        format!("[time] {ms}ms")
    } else if ms < 60_000 {
        let seconds = (ms as f64) / 1_000.0;
        format!("[time] {:.2}s", seconds)
    } else {
        let total_seconds = ms / 1_000;
        let minutes = total_seconds / 60;
        let seconds = total_seconds % 60;
        format!("[time] {minutes}m{seconds}s")
    }
}

fn classify_event_label(event: &Value) -> (&'static str, console::StyledObject<&'static str>) {
    let chunk = extract_chunk_or_result(event).unwrap_or(event);
    if chunk
        .get("toolStreamChunk")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || chunk
            .get("statusUpdate")
            .and_then(|s| s.get("metadata"))
            .and_then(|m| m.get("kind"))
            .and_then(Value::as_str)
            == Some("tool")
    {
        return ("tool", style("[tool]").magenta().bold());
    }
    if chunk.get("statusUpdate").is_some() {
        return ("status", style("[status]").cyan().bold());
    }
    if chunk.get("message").is_some() {
        return ("msg", style("[msg]").blue().bold());
    }
    ("event", style("[event]").dim())
}

fn make_correlation_id(counter: u64) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("corr-{millis}-{counter}")
}

fn validate_target(entries: &[AgentDiscoveryEntry], agent: &str, instance: &str) -> Result<()> {
    let found = entries.iter().any(|entry| {
        (entry.agent_card.name == agent || entry.agent_card.agent_package == agent)
            && entry.agent_card.agent_instance_id == instance
    });
    if found {
        return Ok(());
    }

    let available: Vec<String> = entries
        .iter()
        .map(|e| {
            format!(
                "{} (package={}, instance={})",
                e.agent_card.name, e.agent_card.agent_package, e.agent_card.agent_instance_id
            )
        })
        .collect();

    bail!(
        "Agent target not found: agent='{}', instance='{}'. Available: {}",
        agent,
        instance,
        if available.is_empty() {
            "(none)".to_string()
        } else {
            available.join("; ")
        }
    );
}

async fn list_agents(base_url: &str) -> Result<Vec<AgentDiscoveryEntry>> {
    let base = base_url.trim_end_matches('/');
    let agents_url = format!("{base}/agents");
    let client = reqwest::Client::new();
    let resp = client
        .get(&agents_url)
        .send()
        .await
        .with_context(|| format!("Failed to GET {agents_url}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("List agents failed ({status}) at {agents_url}: {body}");
    }
    serde_json::from_str::<Vec<AgentDiscoveryEntry>>(&body)
        .with_context(|| format!("Invalid /agents response JSON: {body}"))
}

async fn send_message_sse(
    base_url: &str,
    agent: &str,
    instance: &str,
    context_id: &str,
    message_text: &str,
    message_id: &str,
    verbose: bool,
) -> Result<ChatTurnResult> {
    let started = Instant::now();
    let base = base_url.trim_end_matches('/');
    let url = format!("{base}/agents/{agent}/{instance}/a2a/sse");
    let req = JsonRpcRequest {
        jsonrpc: "2.0",
        method: "message.sendStream",
        id: message_id,
        params: SendMessageParams {
            message: Message {
                message_id,
                context_id,
                role: "ROLE_USER",
                parts: vec![Part { text: message_text }],
            },
        },
    };

    let client = reqwest::Client::new();
    let mut resp = client
        .post(&url)
        .header("accept", "text/event-stream")
        .header("content-type", "application/json")
        .json(&req)
        .send()
        .await
        .map_err(|e| tagged_error("NET", format!("Failed to POST chat message to {url}: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(tagged_error(
            "HTTP",
            format!("Chat request failed ({status}) at {url}: {body}"),
        ));
    }

    let mut latest_text = String::new();
    let mut latest_any_text = String::new();
    let mut buffer = String::new();

    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| tagged_error("NET", format!("SSE stream read failed: {e}")))?
    {
        let text = String::from_utf8(chunk.to_vec())
            .map_err(|e| tagged_error("SSE", format!("Invalid UTF-8 in stream: {e}")))?;
        buffer.push_str(&text);

        while let Some(newline_idx) = buffer.find('\n') {
            let line = buffer[..newline_idx].to_string();
            buffer.drain(..=newline_idx);

            let Some(data) = parse_sse_data_line(&line) else {
                continue;
            };
            let event = serde_json::from_str::<Value>(data)
                .map_err(|e| tagged_error("JSON", format!("Invalid SSE JSON payload: {e}")))?;
            if verbose {
                let (_kind, label) = classify_event_label(&event);
                eprintln!("{} {} {}", style("[debug]").dim(), label, event);
            }

            if let Some(rpc_error) = extract_rpc_error(&event) {
                return Err(tagged_error("RPC", rpc_error));
            }

            let chunk_value = extract_chunk_or_result(&event).unwrap_or(&event);
            if let Some(text) = extract_text_from_chunk(chunk_value)
                && !text.trim().is_empty()
            {
                latest_any_text = text.clone();
                if !is_non_user_facing_text(&text) {
                    latest_text = text;
                }
            }

            if let Some(state) = terminal_state_from_chunk(chunk_value)
                && is_failure_state(state)
            {
                let msg = terminal_message_from_chunk(chunk_value)
                    .unwrap_or_else(|| format!("Agent ended in terminal failure state: {state}"));
                return Err(tagged_error("A2A", msg));
            }

            if is_terminal(&event) {
                return Ok(ChatTurnResult {
                    text: if latest_text.is_empty() {
                        latest_any_text
                    } else {
                        latest_text
                    },
                    elapsed_ms: started.elapsed().as_millis(),
                });
            }
        }
    }

    let trailing = buffer.trim();
    if let Some(data) = parse_sse_data_line(trailing) {
        let event = serde_json::from_str::<Value>(data)
            .map_err(|e| tagged_error("JSON", format!("Invalid trailing SSE JSON payload: {e}")))?;
        if let Some(rpc_error) = extract_rpc_error(&event) {
            return Err(tagged_error("RPC", rpc_error));
        }
        let chunk_value = extract_chunk_or_result(&event).unwrap_or(&event);
        if let Some(text) = extract_text_from_chunk(chunk_value)
            && !text.trim().is_empty()
        {
            latest_any_text = text.clone();
            if !is_non_user_facing_text(&text) {
                latest_text = text;
            }
        }
        if let Some(state) = terminal_state_from_chunk(chunk_value)
            && is_failure_state(state)
        {
            let msg = terminal_message_from_chunk(chunk_value)
                .unwrap_or_else(|| format!("Agent ended in terminal failure state: {state}"));
            return Err(tagged_error("A2A", msg));
        }
        if is_terminal(&event) {
            return Ok(ChatTurnResult {
                text: if latest_text.is_empty() {
                    latest_any_text
                } else {
                    latest_text
                },
                elapsed_ms: started.elapsed().as_millis(),
            });
        }
    }

    Ok(ChatTurnResult {
        text: if latest_text.is_empty() {
            latest_any_text
        } else {
            latest_text
        },
        elapsed_ms: started.elapsed().as_millis(),
    })
}

fn classify_error(e: &anyhow::Error) -> (&'static str, String) {
    if let Some(err) = e.downcast_ref::<ChatCommandError>() {
        return (err.tag, err.message.clone());
    }
    ("UNK", e.to_string())
}

pub fn run(agent: &str, base_url: &str, instance: &str, verbose: bool) -> Result<()> {
    let rt = tokio::runtime::Runtime::new().context("Failed to create async runtime")?;
    let agents = rt.block_on(list_agents(base_url))?;
    validate_target(&agents, agent, instance)?;

    let context_id = format!("cli-chat-{agent}-{instance}");
    println!(
        "{}",
        style(format!("Connected to {} (instance: {})", agent, instance))
            .green()
            .bold()
    );
    println!("{}", style(format!("  context_id: {context_id}")).dim());
    println!(
        "{}",
        style("  Type your message and press Enter. Type 'quit' to exit.").dim()
    );
    if verbose {
        println!("{}", style("  verbose: ON").yellow());
    }
    println!();

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut n: u64 = 0;
    let agent_prompt = format!("{agent}>");

    loop {
        print!("{} ", style("you>").cyan().bold());
        stdout.flush()?;

        let mut input = String::new();
        if stdin.read_line(&mut input)? == 0 {
            println!();
            break;
        }
        let input = input.trim();
        if input.is_empty() {
            continue;
        }
        if matches!(input, "quit" | "exit" | "/quit" | "/exit") {
            println!("{}", style("Session ended.").dim());
            break;
        }

        n += 1;
        let correlation_id = make_correlation_id(n);

        if verbose {
            eprintln!(
                "{} sending id={} context_id={}",
                style("[debug]").dim(),
                correlation_id,
                context_id
            );
        }

        print!("{} ", style(&agent_prompt).blue().bold());
        stdout.flush()?;

        let turn_started = Instant::now();
        match rt.block_on(send_message_sse(
            base_url,
            agent,
            instance,
            &context_id,
            input,
            &correlation_id,
            verbose,
        )) {
            Ok(result) => {
                if !result.text.is_empty() {
                    println!("{}", result.text);
                } else {
                    println!("{}", style("(no textual response)").dim());
                }
                if result.elapsed_ms == 0 {
                    println!(
                        "{}",
                        style(format_time_line(turn_started.elapsed().as_millis()))
                            .yellow()
                            .bold()
                    );
                } else {
                    println!(
                        "{}",
                        style(format_time_line(result.elapsed_ms)).yellow().bold()
                    );
                }
            }
            Err(e) => {
                let elapsed_ms = turn_started.elapsed().as_millis();
                let (tag, message) = classify_error(&e);
                println!("{} {}", style(format!("[ERR:{tag}]")).red().bold(), message);
                println!("{}", style(format_time_line(elapsed_ms)).yellow().bold());
            }
        }
        println!();
    }

    Ok(())
}
