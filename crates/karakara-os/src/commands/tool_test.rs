// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! `karak tool test` — run one invocation of a v2 TypeScript tool through the
//! SDK over a stdio DevBackend (the same `serve()` loop + wire as prod, minus
//! the microVM). Builds if needed, spawns node running
//! `serve(tool, stdioTransport())`, sends one `invoke` frame, prints the
//! `result`. Frames are [u32 BE len][utf-8 JSON], matching the SDK's
//! `stdioTransport`.

use std::{
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, anyhow, bail};
use console::style;
use serde_json::{Value, json};

use super::tool_build;

pub fn run(dir: &str, input: Option<&str>) -> Result<()> {
    let dir = Path::new(dir);
    if !dir.is_dir() {
        bail!("Error: not a directory: {}", dir.display());
    }
    tool_build::ensure_built(dir)?;

    let input_value: Value = match input {
        Some(raw) => serde_json::from_str(raw)
            .with_context(|| format!("--input is not valid JSON: {raw}"))?,
        None => json!({}),
    };
    let invoke = json!({
        "t": "invoke",
        "request_id": 1,
        "tool_digest": "dev",
        "input": input_value,
        "deadline_ms": 10000,
    });

    let result = run_once(dir, &invoke)?;
    print_result(&result);
    Ok(())
}

/// Spawn the node DevBackend harness, send one frame, read one result frame.
fn run_once(dir: &Path, invoke: &Value) -> Result<Value> {
    let entry = dir.join("dist/tool.js").canonicalize()?;
    let entry_url = format!("file://{}", entry.display());
    let harness = format!(
        "import {{ serve, stdioTransport }} from \"@karakara/tool\"; \
         import(\"{entry_url}\").then((m) => serve(m.default, stdioTransport())) \
           .catch((e) => {{ console.error(String(e)); process.exit(2); }});"
    );

    let mut child = Command::new("node")
        .args(["--input-type=module", "-e", &harness])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| "spawning node for the DevBackend (is node on PATH?)")?;

    {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(&encode_frame(invoke)?)?;
        // Dropping stdin closes it, so the serve() loop returns after the reply.
    }

    let mut out = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut out)?;
    let status = child.wait()?;
    if !status.success() {
        bail!("Error: tool worker exited with {status}");
    }

    let frame =
        decode_first_frame(&out).ok_or_else(|| anyhow!("no result frame returned by the tool"))?;
    serde_json::from_slice(&frame).context("result frame was not valid JSON")
}

fn print_result(result: &Value) {
    let ok = result.get("ok").and_then(Value::as_bool).unwrap_or(false);
    if ok {
        println!("{} ok", style("✓").green());
        if let Some(output) = result.get("output") {
            println!("  output: {output}");
        }
    } else {
        println!("{} error", style("✗").red());
        if let Some(class) = result.get("error_class") {
            println!("  class:  {class}");
        }
        if let Some(err) = result.get("error") {
            println!("  error:  {err}");
        }
    }
}

/// `[u32 BE len][json]`.
fn encode_frame(value: &Value) -> Result<Vec<u8>> {
    let body = serde_json::to_vec(value)?;
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

/// Extract the first length-prefixed frame's JSON bytes, if present.
fn decode_first_frame(buf: &[u8]) -> Option<Vec<u8>> {
    if buf.len() < 4 {
        return None;
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if buf.len() < 4 + len {
        return None;
    }
    Some(buf[4..4 + len].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let v = json!({"t": "invoke", "request_id": 1});
        let framed = encode_frame(&v).unwrap();
        // 4-byte header + body.
        let body_len = u32::from_be_bytes([framed[0], framed[1], framed[2], framed[3]]) as usize;
        assert_eq!(body_len, framed.len() - 4);
        let decoded = decode_first_frame(&framed).unwrap();
        let back: Value = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn decode_handles_partial_and_empty() {
        assert!(decode_first_frame(&[]).is_none());
        assert!(decode_first_frame(&[0, 0, 0, 10, 1, 2]).is_none()); // claims 10, has 2
    }
}
