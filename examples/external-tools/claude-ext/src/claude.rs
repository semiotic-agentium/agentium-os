use std::{collections::VecDeque, process::ExitStatus, sync::Arc, time::Duration};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::{Mutex, mpsc},
};

#[derive(Debug, Clone)]
pub enum ClaudeEvent {
    Streaming(Vec<Value>),
    TerminalDone(Vec<Value>),
}

#[async_trait]
pub trait ClaudeEngine: Send + Sync {
    async fn send(&self, input: Value) -> Result<(), String>;
    async fn read_next(&self, timeout: Duration) -> Result<Option<ClaudeEvent>, String>;
    async fn close(&self) -> Result<(), String>;
}

#[async_trait]
pub trait ClaudeEngineFactory: Send + Sync {
    async fn create(&self, session_id: &str) -> Result<Arc<dyn ClaudeEngine>, String>;
}

#[derive(Debug, Default)]
pub struct MockClaudeEngine {
    queue: Mutex<VecDeque<ClaudeEvent>>,
}

#[async_trait]
impl ClaudeEngine for MockClaudeEngine {
    async fn send(&self, input: Value) -> Result<(), String> {
        let text = extract_prompt_text(&input);
        if text.trim().is_empty() {
            return Err("session_send requires prompt/content/userInput text".to_string());
        }

        let mut queue = self.queue.lock().await;
        queue.push_back(ClaudeEvent::Streaming(vec![
            json!({
                "kind": "assistant_thinking",
                "thinking": "Analyzing request and preparing a concrete implementation plan."
            }),
            json!({
                "kind": "assistant_text",
                "text": format!("Received task: {}", truncate_for_echo(&text, 220))
            }),
        ]));
        queue.push_back(ClaudeEvent::TerminalDone(vec![
            json!({
                "kind": "assistant_text",
                "text": "Implementation pass complete. Validation checks executed in sandbox session."
            }),
            json!({
                "kind": "terminal_result",
                "subtype": "success",
                "is_error": false,
                "num_turns": 1,
                "total_cost_usd": 0.0,
                "result": "Sandboxed claude-ext session completed successfully."
            }),
        ]));
        Ok(())
    }

    async fn read_next(&self, _timeout: Duration) -> Result<Option<ClaudeEvent>, String> {
        Ok(self.queue.lock().await.pop_front())
    }

    async fn close(&self) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct MockClaudeEngineFactory;

#[async_trait]
impl ClaudeEngineFactory for MockClaudeEngineFactory {
    async fn create(&self, _session_id: &str) -> Result<Arc<dyn ClaudeEngine>, String> {
        Ok(Arc::new(MockClaudeEngine::default()))
    }
}

// ----------------------------------------------------------------------------
// SDK engine: per-Send subprocess design.
//
// claude-code 2.x stream-json mode is NOT a long-lived REPL. Each subprocess
// reads user frames until stdin EOF, processes one turn (assistant reply +
// optional tool calls), emits a `result` frame, and exits. The previous
// "persistent" design tried to keep one claude alive across many Sends and
// hit broken-pipe / silent-exit failures.
//
// New design:
//   * Open: cheap — just stash cwd + host session id, no child yet.
//   * First Send: spawn `claude --output-format stream-json --input-format
//     stream-json ...`, write one user frame, close stdin, drain stdout into
//     an mpsc channel until EOF. Capture claude's own `session_id` from the
//     `system.init` event so we can `--resume` on later sends.
//   * Subsequent Sends: same shape but with `--resume <captured_id>` so
//     claude restores prior conversation context.
//
// read_next pulls events out of the current per-Send channel.
// ----------------------------------------------------------------------------
#[cfg(feature = "sdk-engine")]
#[derive(Debug, Default)]
struct SendDiagnostics {
    stdout_bytes: usize,
    stdout_lines: u64,
    stderr_bytes: usize,
    stderr_lines: u64,
    stdout_preview: Option<String>,
    stderr_preview: Option<String>,
    exit_status: Option<ExitStatus>,
    saw_terminal_result: bool,
}

#[cfg(feature = "sdk-engine")]
#[derive(Debug)]
pub struct SdkClaudeEngine {
    cwd: std::path::PathBuf,
    host_session_id: String,
    /// claude-code session id captured from the first `system.init` event.
    /// Reused via `--resume` on subsequent Sends so the conversation chains.
    claude_session_id: Arc<Mutex<Option<String>>>,
    /// Active per-Send event stream. Each Send replaces this with a fresh
    /// receiver wired to that subprocess's stdout reader task.
    rx: Mutex<Option<mpsc::Receiver<Result<ClaudeEvent, String>>>>,
}

#[cfg(feature = "sdk-engine")]
impl SdkClaudeEngine {
    pub async fn try_new(cwd: std::path::PathBuf, session_id: String) -> Result<Self, String> {
        // We use eprintln as logs because then upsstream we gated them to our log system at microsandbox boundaries
        eprintln!(
            "[cli] open host_session_id={session_id} cwd={}",
            cwd.display()
        );
        fn key_preview(name: &str) -> String {
            match std::env::var(name) {
                Ok(v) if v.is_empty() => "empty".to_string(),
                Ok(v) => {
                    let prefix: String = v.chars().take(8).collect();
                    format!("set len={} prefix={}…", v.len(), prefix)
                }
                Err(_) => "unset".to_string(),
            }
        }
        eprintln!(
            "[cli] env ANTHROPIC_API_KEY={} CLAUDE_CODE_OAUTH_TOKEN={} HOME={} USER={}",
            key_preview("ANTHROPIC_API_KEY"),
            key_preview("CLAUDE_CODE_OAUTH_TOKEN"),
            std::env::var("HOME").unwrap_or_else(|_| "unset".to_string()),
            std::env::var("USER").unwrap_or_else(|_| "unset".to_string()),
        );

        Ok(Self {
            cwd,
            host_session_id: session_id,
            claude_session_id: Arc::new(Mutex::new(None)),
            rx: Mutex::new(None),
        })
    }
}

#[cfg(feature = "sdk-engine")]
#[async_trait]
impl ClaudeEngine for SdkClaudeEngine {
    async fn send(&self, input: Value) -> Result<(), String> {
        use std::process::Stdio;

        let extracted_text = extract_prompt_text(&input);
        if extracted_text.trim().is_empty() {
            return Err("session_send requires prompt/content/userInput text".to_string());
        }

        let text = extracted_text;

        let resume_id = self.claude_session_id.lock().await.clone();

        run_tls_preflight(&self.cwd).await?;

        // Build claude argv. --resume <id> chains into prior turns when present.
        let mut argv = vec![
            "--bare".to_string(),
            "--print".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--verbose".to_string(),
            "--input-format".to_string(),
            "stream-json".to_string(),
            "--dangerously-skip-permissions".to_string(),
            "--permission-mode".to_string(),
            "bypassPermissions".to_string(),
        ];
        if let Some(ref id) = resume_id {
            argv.push("--resume".to_string());
            argv.push(id.clone());
            eprintln!("[cli] send resuming claude_session_id={id}");
        } else {
            eprintln!("[cli] send fresh claude (will capture session_id from system.init)");
        }

        // let use_shell_pipeline = true;

        // // Shell-fed pipeline is the default because it has been the most
        // // reliable path in microsandbox. Keep direct stdin available as an
        // // explicit comparison/debug path with CLAUDE_EXT_USE_SHELL_PIPELINE=0.
        let use_shell_pipeline = std::env::var("CLAUDE_EXT_USE_SHELL_PIPELINE")
            .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
            .unwrap_or(true);

        let mut cmd = if use_shell_pipeline {
            let payload_json = serde_json::to_string(&json!({
                "type": "user",
                "message": {
                    "role": "user",
                    "content": [{"type": "text", "text": text}],
                },
                // Match the older SDK framing and the working probe: always send a
                // session_id on stream-json user frames. On the first turn use the
                // host-managed tool session id; once Claude emits its canonical
                // session id via system.init/result, later turns use that resume id.
                "session_id": resume_id.clone().unwrap_or_else(|| self.host_session_id.clone()),
            }))
            .map_err(|e| classify_cli_error(&format!("serialize shell payload failed: {e}")))?;

            let mut shell_cmd = Command::new("/bin/bash");
            let claude_args = argv
                .iter()
                .map(|s| shlex_quote(s))
                .collect::<Vec<_>>()
                .join(" ");

            // Launch Claude through a shell pipeline instead of Tokio's direct
            // stdin pipe.
            //
            //   printf '%s\n' "$CLAUDE_EXT_STREAM_PAYLOAD" | claude ...
            //
            // `CLAUDE_EXT_STREAM_PAYLOAD` contains exactly one stream-json user
            // frame. `printf` writes that frame plus a terminating newline, then
            // exits, naturally closing the pipe. Claude Code treats that EOF as
            // the end of the turn, processes it, emits stream-json events on
            // stdout, emits a terminal `result` frame, and exits.
            //
            // The stage `echo`s are diagnostics only and are intentionally sent
            // to stderr (`>&2`) so stdout remains clean stream-json for the
            // reader task. `set -o pipefail` and `exit $rc` preserve Claude's
            // failure status at the shell process boundary.
            let shell_script = format!(
                "set -o pipefail; \
                 echo \"[sh] STAGE=start payload_len=${{#CLAUDE_EXT_STREAM_PAYLOAD}}\" >&2; \
                 printf '%s\\n' \"$CLAUDE_EXT_STREAM_PAYLOAD\" \
                 | claude {claude_args}; \
                 rc=$?; \
                 echo \"[sh] STAGE=end claude_exit=$rc\" >&2; \
                 exit $rc"
            );
            shell_cmd.arg("-lc").arg(shell_script);
            shell_cmd.env("CLAUDE_EXT_STREAM_PAYLOAD", payload_json);
            shell_cmd
        } else {
            let mut direct_cmd = Command::new("claude");
            eprintln!("[cli] argv: claude {}", argv.join(" "));
            direct_cmd.args(&argv);
            direct_cmd.stdin(Stdio::piped());
            direct_cmd
        };

        cmd.current_dir(&self.cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        for key in [
            "ANTHROPIC_API_KEY",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "HOME",
            "USER",
            "PATH",
            "NODE_OPTIONS",
            "BUN_FEATURE_FLAG_DISABLE_IPV6",
            "TERM",
            "LANG",
            "LC_ALL",
            "PWD",
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
            "TMPDIR",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
            "NODE_EXTRA_CA_CERTS",
        ] {
            if let Ok(v) = std::env::var(key) {
                cmd.env(key, v);
            }
        }
        // Bind-rootfs/microsandbox execution does not reliably preserve Docker
        // image ENV. Force Claude/Bun TLS and DNS defaults here as well as in
        // the wrapper; otherwise failures surface only after Claude Code's long
        // api_retry backoff loop. Claude Code 2.x is a Bun-compiled native
        // executable, so NODE_OPTIONS alone is not sufficient for DNS family
        // selection; BUN_FEATURE_FLAG_DISABLE_IPV6 is the load-bearing knob.
        apply_claude_process_env(&mut cmd);
        eprintln!(
            "[cli/env] NODE_OPTIONS={} BUN_FEATURE_FLAG_DISABLE_IPV6={} NODE_EXTRA_CA_CERTS={} SSL_CERT_FILE={} SSL_CERT_DIR={}",
            std::env::var("NODE_OPTIONS").unwrap_or_else(|_| "<unset>".to_string()),
            std::env::var("BUN_FEATURE_FLAG_DISABLE_IPV6").unwrap_or_else(|_| "1".to_string()),
            std::env::var("NODE_EXTRA_CA_CERTS").unwrap_or_else(|_| DEFAULT_CA_BUNDLE.to_string()),
            std::env::var("SSL_CERT_FILE").unwrap_or_else(|_| DEFAULT_CA_BUNDLE.to_string()),
            std::env::var("SSL_CERT_DIR").unwrap_or_else(|_| DEFAULT_CA_DIR.to_string()),
        );

        // Forced disable of non-essential traffic (telemetry, auto-update,
        // sentry, statsig). claude-code 2.x hangs ~30s in restricted-network
        // sandboxes waiting on these calls then exits 0 silently in
        // stream-json mode. Setting these turns the wait into a no-op.
        cmd.env("DISABLE_TELEMETRY", "1")
            .env("DISABLE_AUTOUPDATER", "1")
            .env("DISABLE_ERROR_REPORTING", "1")
            .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1");

        let payload = serde_json::to_string(&json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{"type": "text", "text": text}],
            },
            "session_id": resume_id.clone().unwrap_or_else(|| self.host_session_id.clone()),
        }))
        .map_err(|e| classify_cli_error(&format!("serialize user message failed: {e}")))?;

        eprintln!(
            "[cli] send host_session_id={} payload_bytes={}",
            self.host_session_id,
            payload.len()
        );

        let (tx, rx) = mpsc::channel::<Result<ClaudeEvent, String>>(256);
        *self.rx.lock().await = Some(rx);

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                eprintln!("[cli] spawn failed: {e}");
                let message = classify_cli_error(&format!("claude spawn failed: {e}"));
                let _ = tx.send(Err(message.clone())).await;
                return Err(message);
            }
        };

        eprintln!("[cli/spawn] pid={:?}", child.id());

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| classify_cli_error("claude child stdout missing"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| classify_cli_error("claude child stderr missing"))?;

        if !use_shell_pipeline {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| classify_cli_error("claude child stdin missing"))?;

            stdin
                .write_all(payload.as_bytes())
                .await
                .map_err(|e| classify_cli_error(&format!("claude stdin write failed: {e}")))?;
            stdin.write_all(b"\n").await.map_err(|e| {
                classify_cli_error(&format!("claude stdin newline write failed: {e}"))
            })?;
            stdin
                .flush()
                .await
                .map_err(|e| classify_cli_error(&format!("claude stdin flush failed: {e}")))?;
            drop(stdin);
        }

        let diag = Arc::new(Mutex::new(SendDiagnostics::default()));

        let session_id_slot = Arc::clone(&self.claude_session_id);
        let tx_stdout = tx.clone();
        let diag_stdout = Arc::clone(&diag);
        tokio::spawn(async move {
            eprintln!("[cli/stdout] reader entered");
            let mut reader = BufReader::new(stdout).lines();
            loop {
                match reader.next_line().await {
                    Ok(Some(line)) => {
                        let raw_line_bytes = line.len();
                        let line = line.trim().to_string();
                        {
                            let mut diag = diag_stdout.lock().await;
                            diag.stdout_bytes += raw_line_bytes + 1;
                            diag.stdout_lines += 1;
                            if diag.stdout_preview.is_none() && !line.is_empty() {
                                diag.stdout_preview = Some(truncate_for_echo(&line, 400));
                            }
                        }
                        if line.is_empty() {
                            continue;
                        }
                        eprintln!(
                            "[cli/stdout] raw_bytes={} {}",
                            raw_line_bytes,
                            truncate_for_echo(&line, 500)
                        );
                        let value: Value = match serde_json::from_str(&line) {
                            Ok(v) => v,
                            Err(e) => {
                                let _ = tx_stdout
                                    .send(Err(classify_cli_error(&format!(
                                        "parse stream-json line failed: {e}; line={}",
                                        truncate_for_echo(&line, 240)
                                    ))))
                                    .await;
                                break;
                            }
                        };

                        if value.get("type").and_then(Value::as_str) == Some("system")
                            && value.get("subtype").and_then(Value::as_str) == Some("init")
                        {
                            if let Some(sid) = value.get("session_id").and_then(Value::as_str) {
                                let mut guard = session_id_slot.lock().await;
                                if guard.as_deref() != Some(sid) {
                                    eprintln!("[cli] captured claude_session_id={sid}");
                                    *guard = Some(sid.to_string());
                                }
                            }
                        }

                        match parse_stream_json_value(&value) {
                            Ok(Some(event)) => {
                                if matches!(event, ClaudeEvent::TerminalDone(_)) {
                                    let mut diag = diag_stdout.lock().await;
                                    diag.saw_terminal_result = true;
                                }
                                if tx_stdout.send(Ok(event)).await.is_err() {
                                    break;
                                }
                            }
                            Ok(None) => {
                                eprintln!(
                                    "[cli] ignored stream-json line: {}",
                                    truncate_for_echo(&line, 500)
                                );
                            }
                            Err(message) => {
                                let _ = tx_stdout.send(Err(message)).await;
                                break;
                            }
                        }
                    }
                    Ok(None) => {
                        let (
                            stdout_bytes,
                            stdout_lines,
                            stdout_preview,
                            stderr_bytes,
                            stderr_lines,
                            stderr_preview,
                            saw_terminal_result,
                            exit_status,
                        ) = {
                            let diag = diag_stdout.lock().await;
                            (
                                diag.stdout_bytes,
                                diag.stdout_lines,
                                diag.stdout_preview
                                    .clone()
                                    .unwrap_or_else(|| "<none>".to_string()),
                                diag.stderr_bytes,
                                diag.stderr_lines,
                                diag.stderr_preview
                                    .clone()
                                    .unwrap_or_else(|| "<none>".to_string()),
                                diag.saw_terminal_result,
                                diag.exit_status,
                            )
                        };
                        eprintln!(
                            "[cli/stdout] eof stdout_bytes={} stdout_lines={} stdout_preview={} stderr_bytes={} stderr_lines={} stderr_preview={} saw_terminal_result={} exit_status={:?}",
                            stdout_bytes,
                            stdout_lines,
                            stdout_preview,
                            stderr_bytes,
                            stderr_lines,
                            stderr_preview,
                            saw_terminal_result,
                            exit_status,
                        );
                        if !saw_terminal_result {
                            let status = exit_status
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| "unknown".to_string());
                            let _ = tx_stdout
                                .send(Err(classify_cli_error(&format!(
                                    "claude exited before terminal result; status={status} stdout_bytes={stdout_bytes} stdout_lines={stdout_lines} stdout_preview={stdout_preview} stderr_bytes={stderr_bytes} stderr_lines={stderr_lines} stderr_preview={stderr_preview}",
                                ))))
                                .await;
                        }
                        break;
                    }
                    Err(e) => {
                        eprintln!("[cli/stdout] read failed: {e}");
                        let _ = tx_stdout
                            .send(Err(classify_cli_error(&format!(
                                "claude stdout read failed: {e}"
                            ))))
                            .await;
                        break;
                    }
                }
            }
        });

        let diag_stderr = Arc::clone(&diag);
        tokio::spawn(async move {
            eprintln!("[cli/stderr] reader entered");
            let mut reader = BufReader::new(stderr).lines();
            loop {
                match reader.next_line().await {
                    Ok(Some(line)) => {
                        let raw_line_bytes = line.len();
                        let line = line.trim().to_string();
                        {
                            let mut diag = diag_stderr.lock().await;
                            diag.stderr_bytes += raw_line_bytes + 1;
                            diag.stderr_lines += 1;
                            if diag.stderr_preview.is_none() && !line.is_empty() {
                                diag.stderr_preview = Some(truncate_for_echo(&line, 400));
                            }
                        }
                        eprintln!(
                            "[cli/stderr] raw_bytes={} {}",
                            raw_line_bytes,
                            truncate_for_echo(&line, 500)
                        );
                    }
                    Ok(None) => {
                        let diag = diag_stderr.lock().await;
                        eprintln!(
                            "[cli/stderr] eof stderr_bytes={} stderr_lines={} preview={}",
                            diag.stderr_bytes,
                            diag.stderr_lines,
                            diag.stderr_preview.as_deref().unwrap_or("<none>"),
                        );
                        break;
                    }
                    Err(e) => {
                        eprintln!("[cli/stderr] read failed: {e}");
                        break;
                    }
                }
            }
        });

        let diag_wait = Arc::clone(&diag);
        tokio::spawn(async move {
            match child.wait().await {
                Ok(status) => {
                    {
                        let mut diag = diag_wait.lock().await;
                        diag.exit_status = Some(status);
                    }
                    eprintln!("[cli] claude subprocess exited status={status}");
                }
                Err(e) => eprintln!("[cli] claude wait failed: {e}"),
            }
        });

        Ok(())
    }

    async fn read_next(&self, timeout: Duration) -> Result<Option<ClaudeEvent>, String> {
        let mut guard = self.rx.lock().await;
        let rx = match guard.as_mut() {
            Some(rx) => rx,
            None => {
                // No active per-Send subprocess. Honor the caller's timeout so
                // the host FSM does not busy-loop on immediate Open->Read polls.
                drop(guard);
                tokio::time::sleep(timeout).await;
                return Ok(None);
            }
        };
        match tokio::time::timeout(timeout, rx.recv()).await {
            Ok(Some(Ok(event))) => Ok(Some(event)),
            Ok(Some(Err(message))) => Err(message),
            // Channel closed — subprocess EOF reached before session.rs observed
            // a terminal result event for this turn. Treat that as a terminal
            // error, not an empty streaming heartbeat, otherwise the host FSM
            // can loop forever waiting for completion.
            Ok(None) => {
                *guard = None;
                Err(classify_cli_error(
                    "claude subprocess exited before emitting terminal result",
                ))
            }
            Err(_) => Ok(None),
        }
    }

    async fn close(&self) -> Result<(), String> {
        // Per-Send subprocesses self-terminate; nothing persistent to kill.
        // Drop any in-flight receiver so subsequent Sends start clean.
        *self.rx.lock().await = None;
        Ok(())
    }
}

#[cfg(feature = "sdk-engine")]
#[derive(Debug, Clone)]
pub struct SdkClaudeEngineFactory {
    cwd: std::path::PathBuf,
}

#[cfg(feature = "sdk-engine")]
impl SdkClaudeEngineFactory {
    pub fn new(cwd: std::path::PathBuf) -> Self {
        Self { cwd }
    }
}

#[cfg(feature = "sdk-engine")]
#[async_trait]
impl ClaudeEngineFactory for SdkClaudeEngineFactory {
    async fn create(&self, session_id: &str) -> Result<Arc<dyn ClaudeEngine>, String> {
        let engine = SdkClaudeEngine::try_new(self.cwd.clone(), session_id.to_string()).await?;
        Ok(Arc::new(engine))
    }
}

#[cfg(feature = "sdk-engine")]
fn parse_stream_json_value(value: &Value) -> Result<Option<ClaudeEvent>, String> {
    let msg_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match msg_type {
        "assistant" => {
            let mut events = Vec::new();
            if let Some(blocks) = value
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(Value::as_array)
            {
                for block in blocks {
                    match block
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                    {
                        "text" => {
                            if let Some(text) = block.get("text").and_then(Value::as_str)
                                && !text.trim().is_empty()
                            {
                                events.push(json!({ "kind": "assistant_text", "text": text }));
                            }
                        }
                        "thinking" => {
                            if let Some(thinking) = block.get("thinking").and_then(Value::as_str)
                                && !thinking.trim().is_empty()
                            {
                                events.push(json!({
                                    "kind": "assistant_thinking",
                                    "thinking": thinking
                                }));
                            }
                        }
                        "tool_use" => {
                            events.push(json!({
                                "kind": "tool_use",
                                "tool_name": block.get("name").cloned().unwrap_or(Value::Null),
                                "tool_input": block.get("input").cloned().unwrap_or(Value::Null),
                                "tool_use_id": block.get("id").cloned().unwrap_or(Value::Null),
                            }));
                        }
                        "tool_result" => {
                            events.push(json!({
                                "kind": "tool_result",
                                "tool_use_id": block.get("tool_use_id").cloned().unwrap_or(Value::Null),
                                "content": block.get("content").cloned().unwrap_or(Value::Null),
                                "is_error": block.get("is_error").cloned().unwrap_or(Value::Bool(false)),
                            }));
                        }
                        other => {
                            events.push(json!({
                                "kind": "system_notice",
                                "subtype": format!("assistant_block:{other}"),
                                "payload": block,
                            }));
                        }
                    }
                }
            }
            if events.is_empty() {
                Ok(None)
            } else {
                Ok(Some(ClaudeEvent::Streaming(events)))
            }
        }
        "system" => Ok(Some(ClaudeEvent::Streaming(vec![json!({
            "kind": "system_notice",
            "subtype": value.get("subtype").and_then(Value::as_str).unwrap_or("system"),
            "payload": value.clone(),
        })]))),
        "result" => Ok(Some(ClaudeEvent::TerminalDone(vec![json!({
            "kind": "terminal_result",
            "subtype": value.get("subtype").and_then(Value::as_str).unwrap_or("success"),
            "is_error": value.get("is_error").and_then(Value::as_bool).unwrap_or(false),
            "num_turns": value.get("num_turns").and_then(Value::as_u64).unwrap_or(1),
            "total_cost_usd": value.get("total_cost_usd").and_then(Value::as_f64).unwrap_or(0.0),
            "result": value.get("result").and_then(Value::as_str).unwrap_or("Claude session completed"),
            "session_id": value.get("session_id").cloned().unwrap_or(Value::Null),
        })]))),
        "stream_event" => Ok(Some(ClaudeEvent::Streaming(vec![json!({
            "kind": "system_notice",
            "subtype": "stream_event",
            "payload": value.clone(),
        })]))),
        "user" => Ok(None),
        "control_cancel_request" => Ok(None),
        other => Ok(Some(ClaudeEvent::Streaming(vec![json!({
            "kind": "system_notice",
            "subtype": format!("unknown:{other}"),
            "payload": value.clone(),
        })]))),
    }
}

#[cfg(feature = "sdk-engine")]
async fn log_process_snapshot(label: &str, cwd: &std::path::Path) {
    let probe = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        tokio::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(
                "echo '=== proc/self/status (selected) ==='; \
                 grep -E '^(Name|Pid|PPid|Tgid|NSpid|Uid|Gid|Groups|FDSize|State|Threads|TracerPid|NoNewPrivs|Seccomp):' /proc/self/status || true; \
                 echo '=== proc/self/stat ==='; \
                 cat /proc/self/stat; \
                 echo '=== fd ==='; \
                 ls -l /proc/self/fd | sed -n '1,40p'",
            )
            .current_dir(cwd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output(),
    )
    .await;
    match probe {
        Ok(Ok(out)) => eprintln!(
            "[proc/{label}] exit={} stdout={} stderr={}",
            out.status,
            truncate_for_echo(&String::from_utf8_lossy(&out.stdout), 1200),
            truncate_for_echo(&String::from_utf8_lossy(&out.stderr), 600),
        ),
        Ok(Err(e)) => eprintln!("[proc/{label}] spawn failed: {e}"),
        Err(_) => eprintln!("[proc/{label}] timeout after 8s"),
    }
}

#[cfg(feature = "sdk-engine")]
fn classify_cli_error(message: &str) -> String {
    let lowered = message.to_ascii_lowercase();
    if lowered.contains("authentication")
        || lowered.contains("unauthorized")
        || lowered.contains("api key")
        || lowered.contains("login")
        || lowered.contains("oauth")
    {
        return format!("UNAUTHENTICATED: {message}");
    }
    if lowered.contains("timed out")
        || lowered.contains("network")
        || lowered.contains("connection")
        || lowered.contains("temporarily unavailable")
        || lowered.contains("certificate")
        || lowered.contains("tls")
        || lowered.contains("unable_to_verify")
        || lowered.contains("unknown_certificate")
    {
        return format!("UNAVAILABLE: {message}");
    }
    format!("INTERNAL: {message}")
}

pub fn build_engine_factory(cwd: std::path::PathBuf) -> Arc<dyn ClaudeEngineFactory> {
    #[cfg(feature = "sdk-engine")]
    {
        return Arc::new(SdkClaudeEngineFactory::new(cwd));
    }

    #[cfg(not(feature = "sdk-engine"))]
    {
        let _ = cwd;
        Arc::new(MockClaudeEngineFactory)
    }
}

pub fn extract_prompt_text(input: &Value) -> String {
    let mut parts = Vec::new();

    if let Some(prompt) = input.get("prompt").and_then(Value::as_str)
        && !prompt.trim().is_empty()
    {
        parts.push(prompt.trim().to_string());
    }

    match input.get("content") {
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(text) = item.get("text").and_then(Value::as_str)
                    && !text.trim().is_empty()
                {
                    parts.push(text.trim().to_string());
                }
            }
        }
        Some(Value::Object(item)) => {
            if let Some(text) = item.get("text").and_then(Value::as_str)
                && !text.trim().is_empty()
            {
                parts.push(text.trim().to_string());
            }
        }
        _ => {}
    }

    if let Some(user_input) = input.get("userInput") {
        if let Some(text) = user_input.get("display_text").and_then(Value::as_str)
            && !text.trim().is_empty()
        {
            parts.push(text.trim().to_string());
        } else if let Some(text) = user_input.get("prompt").and_then(Value::as_str)
            && !text.trim().is_empty()
        {
            parts.push(text.trim().to_string());
        }
    }

    parts.join("\n")
}

pub fn build_output(hop: u64, events: &[Value], completion: Option<&str>, status: &str) -> Value {
    json!({
        "events": events,
        "completion": completion,
        "historyContext": {
            "hop": hop,
            "op": "page_read",
            "status": status,
            "truncated": false,
            "cursor": Value::Null,
            "payload": {
                "eventCount": events.len(),
                "completion": completion,
            }
        }
    })
}

#[cfg(feature = "sdk-engine")]
async fn run_tls_preflight(cwd: &std::path::Path) -> Result<(), String> {
    use std::process::Stdio;

    const HOST: &str = "api.anthropic.com";
    const SCRIPT: &str = r#"
const tls = require('tls');
const host = 'api.anthropic.com';
const socket = tls.connect({ host, port: 443, servername: host, timeout: 5000 }, () => {
  const cert = socket.getPeerCertificate();
  console.error(`[tls/preflight] authorized=${socket.authorized} authorizationError=${socket.authorizationError || ''} subjectCN=${cert && cert.subject ? cert.subject.CN || '' : ''} issuerCN=${cert && cert.issuer ? cert.issuer.CN || '' : ''}`);
  socket.end();
  process.exit(socket.authorized ? 0 : 2);
});
socket.on('error', (err) => {
  console.error(`[tls/preflight] error code=${err.code || ''} message=${err.message || err}`);
  process.exit(3);
});
socket.on('timeout', () => {
  console.error('[tls/preflight] timeout');
  socket.destroy();
  process.exit(4);
});
"#;

    let mut cmd = Command::new("node");
    cmd.arg("-e")
        .arg(SCRIPT)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for key in [
        "PATH",
        "HOME",
        "USER",
        "NODE_OPTIONS",
        "BUN_FEATURE_FLAG_DISABLE_IPV6",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "NODE_EXTRA_CA_CERTS",
    ] {
        if let Ok(v) = std::env::var(key) {
            cmd.env(key, v);
        }
    }
    apply_claude_process_env(&mut cmd);

    let output = tokio::time::timeout(Duration::from_secs(8), cmd.output())
        .await
        .map_err(|_| classify_cli_error(&format!("TLS preflight to {HOST} timed out after 8s")))?
        .map_err(|e| classify_cli_error(&format!("TLS preflight spawn failed: {e}")))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.success() {
        eprintln!(
            "[tls/preflight] ok stderr={}",
            truncate_for_echo(&stderr, 500)
        );
        return Ok(());
    }

    let cert_file =
        std::env::var("SSL_CERT_FILE").unwrap_or_else(|_| DEFAULT_CA_BUNDLE.to_string());
    let cert_dir = std::env::var("SSL_CERT_DIR").unwrap_or_else(|_| DEFAULT_CA_DIR.to_string());
    let node_extra =
        std::env::var("NODE_EXTRA_CA_CERTS").unwrap_or_else(|_| DEFAULT_CA_BUNDLE.to_string());
    Err(classify_cli_error(&format!(
        "TLS preflight to {HOST} failed before launching Claude (status={}): stderr={} stdout={} SSL_CERT_FILE={} SSL_CERT_DIR={} NODE_EXTRA_CA_CERTS={}. The sandbox rootfs may be missing CA certificates or a corporate MITM root; rebuild with setup_bind_sandbox.sh after adding the required CA to the image/rootfs.",
        output.status,
        truncate_for_echo(&stderr, 900),
        truncate_for_echo(&stdout, 300),
        cert_file,
        cert_dir,
        node_extra,
    )))
}

#[cfg(feature = "sdk-engine")]
const DEFAULT_CA_BUNDLE: &str = "/etc/ssl/certs/ca-certificates.crt";
#[cfg(feature = "sdk-engine")]
const DEFAULT_CA_DIR: &str = "/etc/ssl/certs";

#[cfg(feature = "sdk-engine")]
fn apply_claude_process_env(cmd: &mut Command) {
    cmd.env("NODE_OPTIONS", claude_node_options())
        .env(
            "BUN_FEATURE_FLAG_DISABLE_IPV6",
            std::env::var("BUN_FEATURE_FLAG_DISABLE_IPV6").unwrap_or_else(|_| "1".to_string()),
        )
        .env(
            "SSL_CERT_FILE",
            std::env::var("SSL_CERT_FILE").unwrap_or_else(|_| DEFAULT_CA_BUNDLE.to_string()),
        )
        .env(
            "SSL_CERT_DIR",
            std::env::var("SSL_CERT_DIR").unwrap_or_else(|_| DEFAULT_CA_DIR.to_string()),
        )
        .env(
            "NODE_EXTRA_CA_CERTS",
            std::env::var("NODE_EXTRA_CA_CERTS").unwrap_or_else(|_| DEFAULT_CA_BUNDLE.to_string()),
        );
}

#[cfg(feature = "sdk-engine")]
fn claude_node_options() -> String {
    const IPV4_FIRST: &str = "--dns-result-order=ipv4first";
    const DNS_RESULT_ORDER_PREFIX: &str = "--dns-result-order";

    match std::env::var("NODE_OPTIONS") {
        Ok(existing)
            if existing
                .split_whitespace()
                .any(|opt| opt.starts_with(DNS_RESULT_ORDER_PREFIX)) =>
        {
            existing
        }
        Ok(existing) if !existing.trim().is_empty() => format!("{} {IPV4_FIRST}", existing.trim()),
        _ => IPV4_FIRST.to_string(),
    }
}

fn shlex_quote(text: &str) -> String {
    if text.is_empty() {
        return "''".to_string();
    }
    let escaped = text.replace("'", "'\\''");
    format!("'{escaped}'")
}

pub fn truncate_for_echo(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out = String::new();
    for (i, ch) in text.chars().enumerate() {
        if i >= max_chars {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}
