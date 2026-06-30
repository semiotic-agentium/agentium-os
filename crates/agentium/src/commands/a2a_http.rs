// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Shared HTTP client helpers for A2A chat turns, eval sessions, and ingress publish.

use anyhow::{Context, Result, bail};
use baml_rt_core::{correlation::generate_correlation_id, parse_a2a_sse_json_rpc_chunks};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::utils::{RunnerToken, join_url};

/// Optional runner auth + eval session scope for control-plane HTTP calls.
#[derive(Debug, Clone, Copy)]
pub struct AuthenticatedHttp<'a> {
    pub client: &'a reqwest::Client,
    pub runner_token: Option<&'a RunnerToken>,
    pub eval_session: Option<&'a str>,
}

impl<'a> AuthenticatedHttp<'a> {
    pub fn apply_headers(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let mut req = req;
        if let Some(token) = self.runner_token {
            req = req.header("X-Runner-Token", token.as_str());
        }
        if let Some(session) = self.eval_session {
            req = req.header("X-Agentium-Eval-Session", session);
        }
        req
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<&'a str>,
    role: &'static str,
    parts: Vec<Part<'a>>,
}

#[derive(Debug, Serialize)]
struct Part<'a> {
    text: &'a str,
}

/// Parameters for one `message.sendStream` turn.
pub struct SendStreamParams<'a> {
    pub base_url: &'a str,
    pub agent: &'a str,
    pub instance: &'a str,
    pub context_id: &'a str,
    pub task_id: Option<&'a str>,
    pub text: &'a str,
    pub message_id: Option<&'a str>,
    pub correlation_id: Option<&'a str>,
}

#[derive(Debug)]
pub struct StreamTurnOutcome {
    pub events: Vec<Value>,
    pub states: Vec<String>,
    pub texts: Vec<String>,
    pub task_id: Option<String>,
}

/// POST `/agents/{agent}/{instance}/a2a` and parse SSE JSON-RPC chunks.
pub async fn send_message_stream(
    http: &AuthenticatedHttp<'_>,
    params: &SendStreamParams<'_>,
) -> Result<StreamTurnOutcome> {
    let url = join_url(
        params.base_url,
        &format!("/agents/{}/{}/a2a", params.agent, params.instance),
    );
    let message_id = params
        .message_id
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let correlation_id = params
        .correlation_id
        .map(str::to_owned)
        .unwrap_or_else(|| generate_correlation_id().to_string());

    let rpc_req = JsonRpcRequest {
        jsonrpc: "2.0",
        method: "message.sendStream",
        id: &correlation_id,
        params: SendMessageParams {
            message: Message {
                message_id: &message_id,
                context_id: params.context_id,
                task_id: params.task_id,
                role: "ROLE_USER",
                parts: vec![Part { text: params.text }],
            },
        },
    };

    let req = http.apply_headers(
        http.client
            .post(&url)
            .header("content-type", "application/json")
            .json(&rpc_req),
    );
    let resp = req
        .send()
        .await
        .with_context(|| format!("Failed to POST message.sendStream to {url}"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("A2A failed ({status}) at {url}: {body}");
    }

    let events = parse_a2a_sse_json_rpc_chunks(&body)
        .with_context(|| format!("Invalid /a2a SSE response at {url}"))?;
    if events.is_empty() && !body.trim().is_empty() {
        bail!("A2A returned no SSE events at {url}: {body}");
    }
    if let Some(err) = events.iter().find_map(extract_rpc_error) {
        bail!("A2A RPC error at {url}: {err}");
    }
    let mut states = Vec::new();
    let mut texts = Vec::new();
    let mut task_id = params.task_id.map(str::to_owned);
    for event in &events {
        if let Some(state) = task_state(event) {
            states.push(state);
        }
        if let Some(t) = extract_task_id(event) {
            task_id = Some(t);
        }
        texts.extend(message_texts(event));
    }
    Ok(StreamTurnOutcome {
        events,
        states,
        texts,
        task_id,
    })
}

#[derive(Debug, Deserialize)]
pub struct IngressPublishResponse {
    #[serde(default)]
    pub failures: Vec<Value>,
}

#[derive(Debug)]
pub struct IngressPublishOutcome {
    pub response: IngressPublishResponse,
    pub response_text: String,
}

/// POST `/events/publish` with an ingress fixture body.
pub async fn publish_ingress_fixture(
    http: &AuthenticatedHttp<'_>,
    base_url: &str,
    fixture_body: &Value,
) -> Result<IngressPublishOutcome> {
    let url = join_url(base_url, "/events/publish");
    let req = http.apply_headers(http.client.post(&url).json(fixture_body));
    let resp = req
        .send()
        .await
        .context("ingress POST /events/publish failed")?;
    let status = resp.status();
    let response_text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("ingress publish failed ({status}): {response_text}");
    }
    let response: IngressPublishResponse = serde_json::from_str(&response_text)
        .with_context(|| format!("invalid ingress publish response: {response_text}"))?;
    Ok(IngressPublishOutcome {
        response,
        response_text,
    })
}

#[derive(Deserialize)]
struct EvalSessionResponse {
    eval_session_id: String,
}

/// POST `/eval/sessions` when the runner exposes eval scope (404 → `None`).
pub async fn create_eval_session(
    http: &AuthenticatedHttp<'_>,
    base_url: &str,
    agent: &str,
    model: Option<&str>,
    client: Option<&str>,
) -> Result<Option<String>> {
    let url = join_url(base_url, "/eval/sessions");
    let body = json!({
        "agent": agent,
        "model": model,
        "client": client,
    });
    let req = http.apply_headers(http.client.post(&url).json(&body));
    let resp = req.send().await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Ok(None);
    }
    let parsed: EvalSessionResponse = resp.json().await?;
    Ok(Some(parsed.eval_session_id))
}

pub fn extract_chunk_or_result(value: &Value) -> Option<&Value> {
    let result = value.get("result")?;
    result.get("chunk").or(Some(result))
}

pub fn task_state(chunk: &Value) -> Option<String> {
    let chunk = extract_chunk_or_result(chunk).unwrap_or(chunk);
    chunk
        .get("task")
        .and_then(|t| t.get("status"))
        .and_then(|s| s.get("state"))
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| {
            chunk
                .get("statusUpdate")
                .and_then(|s| s.get("status"))
                .and_then(|s| s.get("state"))
                .and_then(|v| v.as_str())
                .map(String::from)
        })
}

pub fn extract_task_id(chunk: &Value) -> Option<String> {
    let chunk = extract_chunk_or_result(chunk).unwrap_or(chunk);
    chunk
        .get("task")
        .and_then(|t| t.get("id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            chunk
                .get("message")
                .and_then(|m| m.get("taskId"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
}

pub fn message_texts(chunk: &Value) -> Vec<String> {
    let chunk = extract_chunk_or_result(chunk).unwrap_or(chunk);
    let mut out = Vec::new();
    if let Some(msg) = chunk.get("message") {
        collect_text(msg, &mut out);
    }
    if let Some(msg) = chunk.get("artifactUpdate").and_then(|a| a.get("artifact")) {
        collect_text(msg, &mut out);
    }
    out
}

fn collect_text(val: &Value, out: &mut Vec<String>) {
    if let Some(parts) = val.get("parts").and_then(|p| p.as_array()) {
        for part in parts {
            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                out.push(text.to_string());
            }
        }
    }
}

pub fn extract_rpc_error(value: &Value) -> Option<String> {
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

    let mut parts = vec![message.to_string()];
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

pub fn terminal_state_from_chunk(chunk: &Value) -> Option<&str> {
    let chunk = extract_chunk_or_result(chunk).unwrap_or(chunk);
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

pub fn is_terminal(value: &Value) -> bool {
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

pub fn is_failure_state(state: &str) -> bool {
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
