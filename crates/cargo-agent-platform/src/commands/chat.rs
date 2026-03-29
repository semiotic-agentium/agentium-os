//! `chat` subcommand — interactive terminal chat with a deployed agent.

use std::{
    collections::HashMap,
    fmt,
    io::{self, Write},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use baml_rt_core::correlation::generate_correlation_id;
use console::style;
use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest_eventsource::{Event, EventSource};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

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

/// Parameters for an SSE chat turn, passed as one struct to `send_message_sse`.
struct SseRequest<'a> {
    client: &'a reqwest::Client,
    base_url: &'a str,
    agent: &'a str,
    instance: &'a str,
    context_id: &'a str,
    message_text: &'a str,
    message_id: &'a str,
    correlation_id: &'a str,
    verbose: bool,
}

#[derive(Debug)]
struct ChatTurnResult {
    /// Whether any user-facing text was streamed to stdout.
    printed_any: bool,
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

fn extract_agent_message_snapshot(chunk: &Value) -> Option<(String, String)> {
    // Render only the dedicated message channel: chunk.message.parts[*].text.
    let message = chunk.get("message")?;
    let role = message.get("role").and_then(Value::as_str);
    if !matches!(role, Some("ROLE_AGENT") | Some("agent") | Some("assistant")) {
        return None;
    }

    let text = parts_text(message)?;
    let message_id = message
        .get("messageId")
        .and_then(Value::as_str)
        .unwrap_or("agent-message")
        .to_string();

    Some((message_id, text))
}

fn extract_input_required_text(chunk: &Value) -> Option<String> {
    let state = terminal_state_from_chunk(chunk);
    if matches!(
        state,
        Some("TASK_STATE_INPUT_REQUIRED" | "input_required" | "INPUT_REQUIRED")
    ) {
        return chunk
            .get("task")
            .and_then(|t| t.get("status"))
            .and_then(|s| s.get("message"))
            .and_then(parts_text)
            .or_else(|| {
                chunk
                    .get("statusUpdate")
                    .and_then(|s| s.get("status"))
                    .and_then(|s| s.get("message"))
                    .and_then(parts_text)
            });
    }

    None
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
        .and_then(|m| m.as_str().map(ToOwned::to_owned).or_else(|| parts_text(m)))
        .or_else(|| {
            chunk
                .get("statusUpdate")
                .and_then(|s| s.get("status"))
                .and_then(|s| s.get("message"))
                .and_then(|m| m.as_str().map(ToOwned::to_owned).or_else(|| parts_text(m)))
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

/// Sends one chat turn over SSE, streaming response text to `stdout` as it arrives.
///
/// Text chunks are printed incrementally.  When the server sends cumulative
/// snapshots (full message so far on every event), only the new suffix is
/// printed so nothing is duplicated.
async fn send_message_sse(
    req: &SseRequest<'_>,
    stdout: &mut io::Stdout,
    spinner: Option<&ProgressBar>,
) -> Result<ChatTurnResult> {
    let started = Instant::now();
    let base = req.base_url.trim_end_matches('/');
    let url = format!("{base}/agents/{}/{}/a2a/sse", req.agent, req.instance);

    let rpc_req = JsonRpcRequest {
        jsonrpc: "2.0",
        method: "message.sendStream",
        id: req.correlation_id,
        params: SendMessageParams {
            message: Message {
                message_id: req.message_id,
                context_id: req.context_id,
                role: "ROLE_USER",
                parts: vec![Part { text: req.message_text }],
            },
        },
    };

    let request = req
        .client
        .post(&url)
        .header("accept", "text/event-stream")
        .header("content-type", "application/json")
        .json(&rpc_req);

    let mut es = EventSource::new(request)
        .map_err(|e| tagged_error("SSE", format!("Cannot build SSE request: {e}")))?;

    // Track the last full snapshot per messageId, so we can print only deltas
    // from chunk.message.parts[*].text without duplicating cumulative snapshots.
    let mut printed_by_message_id: HashMap<String, String> = HashMap::new();
    let mut active_message_id: Option<String> = None;
    let mut printed_any = false;

    loop {
        // Intentionally no idle timeout here.
        // Agent turns may include multiple downstream network/tool calls and can be silent
        // for longer periods while still making valid progress.
        // Keep the stream open until terminal event, transport close, or explicit error.
        let event_opt = es.next().await;

        let Some(event_result) = event_opt else {
            // Stream ended without a terminal event.
            break;
        };

        match event_result {
            Ok(Event::Open) => {}
            Ok(Event::Message(msg)) => {
                let event = serde_json::from_str::<Value>(&msg.data).map_err(|e| {
                    tagged_error("JSON", format!("Invalid SSE JSON payload: {e}"))
                })?;

                if req.verbose {
                    let (_kind, label) = classify_event_label(&event);
                    let debug_line = format!("{} {} {}", style("[debug]").dim(), label, event);
                    if let Some(pb) = spinner {
                        // Print above active spinner to avoid ANSI line-clobbering on stderr.
                        pb.println(debug_line);
                    } else {
                        eprintln!("{debug_line}");
                    }
                }

                if let Some(rpc_error) = extract_rpc_error(&event) {
                    if printed_any {
                        writeln!(stdout)?;
                    }
                    es.close();
                    return Err(tagged_error("RPC", rpc_error));
                }

                let chunk_value = extract_chunk_or_result(&event).unwrap_or(&event);

                if let Some((message_id, text)) = extract_agent_message_snapshot(chunk_value)
                    && !text.trim().is_empty()
                {
                    if active_message_id.as_ref() != Some(&message_id) && printed_any {
                        writeln!(stdout)?;
                    }

                    let previous = printed_by_message_id
                        .entry(message_id.clone())
                        .or_default()
                        .clone();

                    let delta: &str = if text.starts_with(previous.as_str()) {
                        &text[previous.len()..]
                    } else {
                        // Same message id but non-prefix update; print full snapshot on a new line.
                        if !previous.is_empty() {
                            writeln!(stdout)?;
                        }
                        &text
                    };

                    if !delta.is_empty() {
                        if !printed_any
                            && let Some(pb) = spinner
                        {
                            pb.finish_and_clear();
                        }
                        write!(stdout, "{delta}")?;
                        stdout.flush()?;
                        printed_by_message_id.insert(message_id.clone(), text);
                        active_message_id = Some(message_id);
                        printed_any = true;
                    }
                } else if let Some(text) = extract_input_required_text(chunk_value)
                    && !text.trim().is_empty()
                {
                    if !printed_any
                        && let Some(pb) = spinner
                    {
                        pb.finish_and_clear();
                    }
                    write!(stdout, "{text}")?;
                    stdout.flush()?;
                    printed_any = true;
                }

                if let Some(state) = terminal_state_from_chunk(chunk_value)
                    && is_failure_state(state)
                {
                    let failure_msg = terminal_message_from_chunk(chunk_value)
                        .unwrap_or_else(|| {
                            format!("Agent ended in terminal failure state: {state}")
                        });
                    if printed_any {
                        writeln!(stdout)?;
                    }
                    es.close();
                    return Err(tagged_error("A2A", failure_msg));
                }

                if is_terminal(&event) {
                    if printed_any {
                        writeln!(stdout)?;
                    }
                    es.close();
                    return Ok(ChatTurnResult {
                        printed_any,
                        elapsed_ms: started.elapsed().as_millis(),
                    });
                }
            }
            Err(reqwest_eventsource::Error::StreamEnded) => {
                // The SSE transport closed cleanly; treat as normal end-of-stream.
                break;
            }
            Err(e) => {
                if printed_any {
                    writeln!(stdout)?;
                }
                es.close();
                return Err(tagged_error("SSE", e.to_string()));
            }
        }
    }

    if printed_any {
        writeln!(stdout)?;
    }
    Ok(ChatTurnResult {
        printed_any,
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

    // Session-scoped UUID: stable across all turns in this CLI session.
    let context_id = Uuid::new_v4().to_string();

    // One client for the whole session — avoids TCP handshake overhead per turn
    // and sets a connection timeout so a hung server fails fast.
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .build()
        .context("Failed to build HTTP client")?;

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

        // A2A expects JSON-RPC id in correlation format: corr-<millis>-<counter>.
        let correlation_id = generate_correlation_id().to_string();
        // Message id can be any stable per-turn id.
        let message_id = Uuid::new_v4().to_string();

        if verbose {
            eprintln!(
                "{} sending correlation_id={} message_id={} context_id={}",
                style("[debug]").dim(),
                correlation_id,
                message_id,
                context_id
            );
        }

        print!("{} ", style(&agent_prompt).blue().bold());
        stdout.flush()?;

        let sse_req = SseRequest {
            client: &client,
            base_url,
            agent,
            instance,
            context_id: &context_id,
            message_text: input,
            message_id: &message_id,
            correlation_id: &correlation_id,
            verbose,
        };

        let spinner = if console::user_attended_stderr() {
            let pb = ProgressBar::new_spinner();
            let style = ProgressStyle::with_template("{spinner:.cyan} {msg}")
                .context("Failed to create spinner style")?;
            pb.set_style(style);
            pb.set_message(format!("{agent} is thinking…"));
            pb.enable_steady_tick(Duration::from_millis(80));
            Some(pb)
        } else {
            None
        };

        let turn_started = Instant::now();
        match rt.block_on(send_message_sse(&sse_req, &mut stdout, spinner.as_ref())) {
            Ok(result) => {
                if let Some(pb) = &spinner {
                    pb.finish_and_clear();
                }
                if !result.printed_any {
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
                if let Some(pb) = &spinner {
                    pb.finish_and_clear();
                }
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
