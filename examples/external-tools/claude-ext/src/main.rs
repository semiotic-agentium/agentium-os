mod claude;
mod session;
mod types;

use std::sync::Arc;

use claude::build_engine_factory;
use serde_json::{Value, json};
use session::SessionStore;
use tokio::{
    io::{self, AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
};
use types::*;

const INPUT_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "prompt": { "type": "string" },
    "content": {
      "oneOf": [
        {
          "type": "array",
          "items": {
            "oneOf": [
              {
                "type": "object",
                "properties": {
                  "kind": { "type": "string", "const": "text" },
                  "text": { "type": "string" }
                },
                "required": ["kind", "text"],
                "additionalProperties": false
              },
              {
                "type": "object",
                "properties": {
                  "kind": { "type": "string", "const": "image_url" },
                  "url": { "type": "string" }
                },
                "required": ["kind", "url"],
                "additionalProperties": false
              },
              {
                "type": "object",
                "properties": {
                  "kind": { "type": "string", "const": "image_base64" },
                  "media_type": { "type": "string" },
                  "data": { "type": "string" }
                },
                "required": ["kind", "media_type", "data"],
                "additionalProperties": false
              }
            ]
          }
        },
        {
          "oneOf": [
            {
              "type": "object",
              "properties": {
                "kind": { "type": "string", "const": "text" },
                "text": { "type": "string" }
              },
              "required": ["kind", "text"],
              "additionalProperties": false
            },
            {
              "type": "object",
              "properties": {
                "kind": { "type": "string", "const": "image_url" },
                "url": { "type": "string" }
              },
              "required": ["kind", "url"],
              "additionalProperties": false
            },
            {
              "type": "object",
              "properties": {
                "kind": { "type": "string", "const": "image_base64" },
                "media_type": { "type": "string" },
                "data": { "type": "string" }
              },
              "required": ["kind", "media_type", "data"],
              "additionalProperties": false
            }
          ]
        }
      ]
    },
    "userInput": {
      "type": "object",
      "properties": {
        "display_text": { "type": "string" },
        "prompt": { "type": "string" }
      },
      "additionalProperties": false
    }
  },
  "additionalProperties": false
}"#;

const OUTPUT_SCHEMA: &str = r#"{
  "type": "object",
  "properties": {
    "events": { "type": "array", "items": { "type": "object" } },
    "completion": { "type": "string", "enum": ["DONE", "INPUT_REQUIRED", "INTERRUPTED"] },
    "historyContext": {
      "type": "object",
      "properties": {
        "hop": { "type": "integer" },
        "op": { "type": "string" },
        "status": { "type": "string" },
        "truncated": { "type": "boolean" },
        "cursor": { "type": ["string", "null"] },
        "payload": {
          "type": "object",
          "properties": {
            "eventCount": { "type": "integer" },
            "completion": { "type": ["string", "null"] }
          },
          "additionalProperties": false
        }
      },
      "additionalProperties": false
    }
  },
  "additionalProperties": false
}"#;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    // Microsandbox boots the adapter with HOME=/ and USER unset. claude-code
    // (and many other CLIs) treat HOME=/ as "no real home" and silently hang
    // on first-run config init at $HOME/.claude/. Pin a real per-tool home
    // before any subprocess (including the SDK-spawned claude CLI) reads env.
    ensure_runtime_home();
    log_secret_presence();

    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/tmp"));
    let engine_factory = build_engine_factory(cwd);
    let sessions = Arc::new(
        SessionStore::default()
            .with_engine_factory(engine_factory)
            .with_idle_ttl(None),
    );
    sessions.clone().start_idle_reaper();

    let (tx, mut rx) = mpsc::channel::<Value>(256);

    let writer = tokio::spawn(async move {
        let mut stdout = io::stdout();
        while let Some(response) = rx.recv().await {
            if write_framed_response(&mut stdout, &response).await.is_err() {
                break;
            }
        }
    });

    let mut stdin = io::stdin();
    loop {
        let Some(request) = read_framed_request(&mut stdin).await else {
            break;
        };

        let sessions = Arc::clone(&sessions);
        let tx = tx.clone();
        tokio::spawn(async move {
            let response = handle_request(request, sessions).await;
            let _ = tx.send(response).await;
        });
    }

    drop(tx);
    let _ = writer.await;
}

async fn handle_request(request: Value, sessions: Arc<SessionStore>) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));

    match method {
        METHOD_DESCRIBE => ok(
            id,
            json!({
                "protocol_version": PROTOCOL_VERSION,
                "tool_name": TOOL_NAME,
                "supported_methods": SUPPORTED_METHODS,
                "schema_digest": "sha256:5bca5f9e64313cbcad5d89fb2907f466434e24f6203c6ffd2aa4f798ac7f086b",
            }),
        ),
        METHOD_SCHEMA => {
            let input: Value =
                serde_json::from_str(INPUT_SCHEMA).unwrap_or_else(|_| json!({"type": "object"}));
            let output: Value =
                serde_json::from_str(OUTPUT_SCHEMA).unwrap_or_else(|_| json!({"type": "object"}));
            ok(
                id,
                json!({
                    "schema_version": 1,
                    "tool_name": TOOL_NAME,
                    "content_type": "application/schema+json",
                    "content_digest": "sha256:5bca5f9e64313cbcad5d89fb2907f466434e24f6203c6ffd2aa4f798ac7f086b",
                    "input": input,
                    "output": output,
                }),
            )
        }
        METHOD_SESSION_OPEN => sessions.open(id, params).await,
        METHOD_SESSION_SEND => sessions.send(id, params).await,
        METHOD_SESSION_READ => sessions.read(id, params).await,
        METHOD_SESSION_FINISH => sessions.finish(id, params).await,
        METHOD_SESSION_ABORT => sessions.abort(id, params).await,
        _ => err(
            id,
            ERR_METHOD_NOT_FOUND,
            format!("method not found: {method}"),
            "invalid_argument",
        ),
    }
}

/// Make `HOME` / `USER` sane and pin a real workspace cwd before any tool
/// subprocess runs.
///
/// Microsandbox launches the adapter with `HOME=/`, `USER` unset, and
/// `cwd=/`. claude-code — and most CLIs that maintain per-user config plus a
/// project workspace — refuse to bootstrap state at the rootfs root and hang
/// waiting for first-run setup that never completes. Three fixes, all in one
/// pass:
///
/// 1. Re-point `HOME` at `/home/sandbox/` and pre-create the canonical config
///    dirs. `/home/sandbox` is the non-root home baked into the adapter image
///    (claude-code refuses `--dangerously-skip-permissions` under root).
/// 2. Fill in `USER` so anything that introspects either gets a real answer.
/// 3. `chdir` into `$HOME/workspace` so the SDK's default `cwd` (forwarded as
///    the claude-code workspace) is a real, writable project dir instead of
///    `/`.
///
/// Safe to call from `main` because the process is single-threaded at that
/// point — no other thread is reading env or cwd yet, so `set_var`'s race
/// risk (Edition 2024 unsafe contract) does not apply here.
fn ensure_runtime_home() {
    const DEFAULT_HOME: &str = "/home/sandbox";
    const DEFAULT_USER: &str = "sandbox";

    let home = match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && h != "/" => std::path::PathBuf::from(h),
        _ => {
            let fallback = std::path::PathBuf::from(DEFAULT_HOME);
            // SAFETY: single-threaded at process startup; `set_var`'s
            // race-vs-readers contract (Rust Edition 2024) is satisfied — no
            // concurrent env access.
            unsafe {
                std::env::set_var("HOME", &fallback);
            }
            fallback
        }
    };
    let xdg_config_home = home.join(".config");
    let xdg_cache_home = home.join(".cache");
    let _ = std::fs::create_dir_all(&home);
    let _ = std::fs::create_dir_all(home.join(".claude"));
    let _ = std::fs::create_dir_all(&xdg_config_home);
    let _ = std::fs::create_dir_all(&xdg_cache_home);

    if std::env::var("USER").map(|u| u.is_empty()).unwrap_or(true) {
        // SAFETY: same justification as the HOME write above.
        unsafe {
            std::env::set_var("USER", DEFAULT_USER);
        }
    }

    // Anchor the SDK's workspace at a real dir. `/` triggers claude-code's
    // workspace bootstrapper to scan an entire rootfs for a project anchor and
    // hang on first-run setup; a dedicated dir avoids that path entirely.
    let workspace = home.join("workspace");
    if std::fs::create_dir_all(&workspace).is_ok() {
        let _ = std::env::set_current_dir(&workspace);
        // SAFETY: same startup-only env mutation conditions as HOME/USER.
        unsafe {
            std::env::set_var("PWD", &workspace);
        }
    }

    if std::env::var("XDG_CONFIG_HOME")
        .map(|v| v.is_empty())
        .unwrap_or(true)
    {
        // SAFETY: same startup-only env mutation conditions as HOME/USER.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &xdg_config_home);
        }
    }
    if std::env::var("XDG_CACHE_HOME")
        .map(|v| v.is_empty())
        .unwrap_or(true)
    {
        // SAFETY: same startup-only env mutation conditions as HOME/USER.
        unsafe {
            std::env::set_var("XDG_CACHE_HOME", &xdg_cache_home);
        }
    }
    if std::env::var("TMPDIR")
        .map(|v| v.is_empty())
        .unwrap_or(true)
    {
        // SAFETY: same startup-only env mutation conditions as HOME/USER.
        unsafe {
            std::env::set_var("TMPDIR", "/tmp");
        }
    }

    // Surface effective identity so sandbox-launch quirks (USER override,
    // missing setuid drop, etc.) show up in the adapter stderr log next to
    // the secret presence line.
    eprintln!(
        "[adapter] runtime home={} user={} uid={} gid={} cwd={}",
        home.display(),
        std::env::var("USER").unwrap_or_else(|_| "unset".to_string()),
        unsafe { libc_getuid() },
        unsafe { libc_getgid() },
        std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "?".to_string()),
    );
}

unsafe extern "C" {
    fn getuid() -> u32;
    fn getgid() -> u32;
}

unsafe fn libc_getuid() -> u32 {
    unsafe { getuid() }
}

unsafe fn libc_getgid() -> u32 {
    unsafe { getgid() }
}

/// One-shot startup probe of secret env vars. Prints presence (set / empty /
/// unset) — never the value — so operators can confirm the secret pipeline
/// without leaking material. Output goes to stderr, surfaces under the
/// runner's `sandbox tool-adapter stderr` log line via the recv pump.
fn log_secret_presence() {
    fn presence(name: &str) -> &'static str {
        match std::env::var(name) {
            Ok(v) if v.is_empty() => "empty",
            Ok(_) => "set",
            Err(_) => "unset",
        }
    }
    eprintln!(
        "[adapter] secrets boot ANTHROPIC_API_KEY={} CLAUDE_CODE_OAUTH_TOKEN={}",
        presence("ANTHROPIC_API_KEY"),
        presence("CLAUDE_CODE_OAUTH_TOKEN"),
    );
}

async fn read_framed_request(stdin: &mut io::Stdin) -> Option<Value> {
    let mut header = [0u8; 4];
    match stdin.read_exact(&mut header).await {
        Ok(_) => {}
        Err(_) => return None,
    }
    let len = u32::from_be_bytes(header) as usize;

    let mut payload = vec![0u8; len];
    if stdin.read_exact(&mut payload).await.is_err() {
        return Some(err(
            Value::Null,
            ERR_PARSE_ERROR,
            "short framed request payload",
            "invalid_argument",
        ));
    }

    match serde_json::from_slice::<Value>(&payload) {
        Ok(v) => Some(v),
        Err(e) => Some(err(
            Value::Null,
            ERR_PARSE_ERROR,
            format!("invalid framed JSON request: {e}"),
            "invalid_argument",
        )),
    }
}

async fn write_framed_response(stdout: &mut io::Stdout, response: &Value) -> io::Result<()> {
    let bytes = serde_json::to_vec(response)?;
    let len = (bytes.len() as u32).to_be_bytes();
    stdout.write_all(&len).await?;
    stdout.write_all(&bytes).await?;
    stdout.flush().await
}
