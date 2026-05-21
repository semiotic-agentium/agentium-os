//! Stdio wrapper around the in-memory fake MCP server. Reads a JSON config
//! file path from argv and serves over its own stdin/stdout. Test-only.

use std::{process::ExitCode, time::Duration};

use baml_rt_mcp::fixture::{FakeMcpConfig, new_state, run_fake_server};

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let path = match std::env::args().nth(1) {
        Some(path) => path,
        None => {
            eprintln!("usage: fake-mcp-stdio <config.json>");
            return ExitCode::from(2);
        }
    };
    let raw = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("failed to read config `{path}`: {err}");
            return ExitCode::from(2);
        }
    };
    let config: FakeMcpConfig = match serde_json::from_slice(&raw) {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("invalid config: {err}");
            return ExitCode::from(2);
        }
    };
    if config.stderr_spam_mode {
        tokio::spawn(async {
            loop {
                eprintln!("fake-mcp-stderr-spam {}", "x".repeat(4096));
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        });
    }
    let state = new_state();
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    if let Err(err) = run_fake_server(config, state, stdin, stdout).await {
        eprintln!("fake server error: {err}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
