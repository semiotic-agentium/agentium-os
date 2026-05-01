//! `chat` subcommand — interactive terminal chat with a deployed agent.

use std::{
    collections::HashMap,
    fmt,
    io::{self, Write},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use baml_rt_core::{correlation::generate_correlation_id, parse_a2a_sse_json_rpc_chunks};
use console::style;
use indicatif::{ProgressBar, ProgressStyle};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use super::{
    agent_discovery::AgentDiscoveryEntry,
    utils::{build_http_client, join_url},
};

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
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<&'a str>,
    role: &'static str,
    parts: Vec<Part<'a>>,
}

#[derive(Debug, Serialize)]
struct Part<'a> {
    text: &'a str,
}

/// Parameters for one chat turn request.
struct SseRequest<'a> {
    client: &'a reqwest::Client,
    base_url: &'a str,
    agent: &'a str,
    instance: &'a str,
    context_id: &'a str,
    task_id: Option<&'a str>,
    message_text: &'a str,
    message_id: &'a str,
    correlation_id: &'a str,
    verbose: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AwaitingState {
    InputRequired,
    AuthRequired,
}

#[derive(Debug)]
struct ChatTurnResult {
    /// Whether any user-facing text was streamed to stdout.
    printed_any: bool,
    /// Non-terminal boundary where the agent is waiting for user follow-up.
    awaiting_state: Option<AwaitingState>,
    /// Latest observed task id from streamed chunks (if provided by server).
    task_id: Option<String>,
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

fn waiting_state_from_chunk(chunk: &Value) -> Option<AwaitingState> {
    match terminal_state_from_chunk(chunk) {
        Some("TASK_STATE_INPUT_REQUIRED" | "input_required" | "INPUT_REQUIRED") => {
            Some(AwaitingState::InputRequired)
        }
        Some("TASK_STATE_AUTH_REQUIRED" | "auth_required" | "AUTH_REQUIRED") => {
            Some(AwaitingState::AuthRequired)
        }
        _ => None,
    }
}

fn extract_waiting_prompt_text(chunk: &Value) -> Option<String> {
    waiting_state_from_chunk(chunk)?;

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

fn extract_task_id_from_chunk(chunk: &Value) -> Option<String> {
    chunk
        .get("task")
        .and_then(|t| t.get("id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            chunk
                .get("statusUpdate")
                .and_then(|s| s.get("status"))
                .and_then(|s| s.get("taskId"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            chunk
                .get("statusUpdate")
                .and_then(|s| s.get("taskId"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            chunk
                .get("message")
                .and_then(|m| m.get("taskId"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            chunk
                .get("taskId")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
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

fn terminal_state_from_chunk(chunk: &Value) -> Option<&str> {
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

async fn list_agents(client: &reqwest::Client, base_url: &str) -> Result<Vec<AgentDiscoveryEntry>> {
    let agents_url = join_url(base_url, "/agents");
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

/// Sends one chat turn over HTTP JSON-RPC (`/a2a`), then renders collected chunks.
///
/// The runner streams SSE (`text/event-stream`): each event `data:` line is one JSON-RPC item.
/// We process them in-order and render text snapshots as deltas to avoid duplicated cumulative output.
async fn send_message_http(
    req: &SseRequest<'_>,
    stdout: &mut io::Stdout,
    spinner: Option<&ProgressBar>,
) -> Result<ChatTurnResult> {
    let started = Instant::now();
    let url = join_url(
        req.base_url,
        &format!("/agents/{}/{}/a2a", req.agent, req.instance),
    );

    let rpc_req = JsonRpcRequest {
        jsonrpc: "2.0",
        method: "message.sendStream",
        id: req.correlation_id,
        params: SendMessageParams {
            message: Message {
                message_id: req.message_id,
                context_id: req.context_id,
                task_id: req.task_id,
                role: "ROLE_USER",
                parts: vec![Part {
                    text: req.message_text,
                }],
            },
        },
    };

    let response = req
        .client
        .post(&url)
        .header("content-type", "application/json")
        .json(&rpc_req)
        .send()
        .await
        .with_context(|| format!("Failed to POST chat turn to {url}"))?;

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(tagged_error(
            "HTTP",
            format!("Chat request failed ({status}) at {url}: {body}"),
        ));
    }

    let events: Vec<Value> = parse_a2a_sse_json_rpc_chunks(&body)
        .map_err(|e| tagged_error("SSE", format!("Invalid /a2a SSE response: {e}")))?;

    let mut printed_by_message_id: HashMap<String, String> = HashMap::new();
    let mut active_message_id: Option<String> = None;
    let mut printed_any = false;
    let mut awaiting_state: Option<AwaitingState> = None;
    let mut latest_task_id = req.task_id.map(ToOwned::to_owned);

    for event in events {
        if req.verbose {
            let (_kind, label) = classify_event_label(&event);
            let debug_line = format!("{} {} {}", style("[debug]").dim(), label, event);
            if let Some(pb) = spinner {
                pb.println(debug_line);
            } else {
                eprintln!("{debug_line}");
            }
        }

        if let Some(rpc_error) = extract_rpc_error(&event) {
            if printed_any {
                writeln!(stdout)?;
            }
            return Err(tagged_error("RPC", rpc_error));
        }

        let chunk_value = extract_chunk_or_result(&event).unwrap_or(&event);

        if let Some(task_id) = extract_task_id_from_chunk(chunk_value) {
            latest_task_id = Some(task_id);
        }
        if let Some(state) = waiting_state_from_chunk(chunk_value) {
            awaiting_state = Some(state);
        }

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
                if !previous.is_empty() {
                    writeln!(stdout)?;
                }
                &text
            };

            if !delta.is_empty() {
                if !printed_any && let Some(pb) = spinner {
                    pb.finish_and_clear();
                }
                write!(stdout, "{delta}")?;
                stdout.flush()?;
                printed_by_message_id.insert(message_id.clone(), text);
                active_message_id = Some(message_id);
                printed_any = true;
            }
        } else if let Some(text) = extract_waiting_prompt_text(chunk_value)
            && !text.trim().is_empty()
        {
            if !printed_any && let Some(pb) = spinner {
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
                .unwrap_or_else(|| format!("Agent ended in terminal failure state: {state}"));
            if printed_any {
                writeln!(stdout)?;
            }
            return Err(tagged_error("A2A", failure_msg));
        }

        if is_terminal(&event) {
            if printed_any {
                writeln!(stdout)?;
            }
            return Ok(ChatTurnResult {
                printed_any,
                awaiting_state,
                task_id: latest_task_id,
                elapsed_ms: started.elapsed().as_millis(),
            });
        }
    }

    if printed_any {
        writeln!(stdout)?;
    }
    Ok(ChatTurnResult {
        printed_any,
        awaiting_state,
        task_id: latest_task_id,
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

    // One client for discovery + chat turns.
    let client = build_http_client(Some(Duration::from_secs(10)))?;

    let agents = rt.block_on(list_agents(&client, base_url))?;
    validate_target(&agents, agent, instance)?;

    // Session-scoped UUID: stable across all turns in this CLI session.
    let context_id = Uuid::new_v4().to_string();

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
    let mut task_id: Option<String> = None;

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
                "{} sending correlation_id={} message_id={} context_id={} task_id={}",
                style("[debug]").dim(),
                correlation_id,
                message_id,
                context_id,
                task_id.as_deref().unwrap_or("(none)"),
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
            task_id: task_id.as_deref(),
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
        match rt.block_on(send_message_http(&sse_req, &mut stdout, spinner.as_ref())) {
            Ok(result) => {
                if let Some(pb) = &spinner {
                    pb.finish_and_clear();
                }
                let ChatTurnResult {
                    printed_any,
                    awaiting_state,
                    task_id: observed_task_id,
                    elapsed_ms,
                } = result;

                if let Some(next_task_id) = observed_task_id {
                    let task_changed = task_id.as_deref() != Some(next_task_id.as_str());
                    if verbose && task_changed {
                        eprintln!(
                            "{} observed task_id={} (updated)",
                            style("[debug]").dim(),
                            next_task_id
                        );
                    }
                    task_id = Some(next_task_id);
                }

                if !printed_any {
                    match awaiting_state {
                        Some(AwaitingState::InputRequired) => {
                            println!("{}", style("(awaiting input)").cyan().bold())
                        }
                        Some(AwaitingState::AuthRequired) => {
                            println!("{}", style("(authentication required)").yellow().bold())
                        }
                        None => println!("{}", style("(no textual response)").dim()),
                    }
                }
                if elapsed_ms == 0 {
                    println!(
                        "{}",
                        style(format_time_line(turn_started.elapsed().as_millis()))
                            .yellow()
                            .bold()
                    );
                } else {
                    println!("{}", style(format_time_line(elapsed_ms)).yellow().bold());
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
