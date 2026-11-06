// SPDX-FileCopyrightText: 2026 Karakara Labs
// SPDX-License-Identifier: Apache-2.0

//! `karak tool test` runs an MCP toolset through the real supervisor DevBackend.

use std::{
    io::{BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use baml_rt_tools::external_tools::{KtoolRuntimeTier, audit_ktool_bundle};
use baml_sandbox_protocol::{
    MAX_FRAME_BYTES,
    protocol_v2::{CallContext, GuestMessage, HostMessage, InvokeRequest, PROTOCOL_V2_VERSION},
};
use console::style;
use serde_json::{Value, json};

use super::tool_publish;

pub fn run(dir: &str, input: Option<&str>) -> Result<()> {
    let dir = Path::new(dir);
    if !dir.is_dir() {
        bail!("Error: not a directory: {}", dir.display());
    }
    let bundle = tool_publish::build_bundle(dir, KtoolRuntimeTier::Sandbox)?.bytes;
    let input_value: Value = match input {
        Some(raw) => serde_json::from_str(raw)
            .with_context(|| format!("--input is not valid JSON: {raw}"))?,
        None => json!({}),
    };
    let result = run_once(&bundle, input_value)?;
    print_result(&result);
    Ok(())
}

fn run_once(bundle: &[u8], input: Value) -> Result<Value> {
    let supervisor = supervisor_binary()?;
    let backend = std::env::var("KARAKARA_SUPERVISOR_BACKEND").unwrap_or_else(|_| "bwrap".into());
    let mut child = Command::new(&supervisor)
        .args(["--dev", "--backend", &backend])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| {
            format!(
                "spawning the guest supervisor ({}) for the DevBackend",
                supervisor.display()
            )
        })?;
    let result = drive(&mut child, bundle, input);
    for _ in 0..250 {
        match child.try_wait() {
            Ok(Some(_)) => return result,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn drive(child: &mut Child, bundle: &[u8], input: Value) -> Result<Value> {
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("no supervisor stdin"))?;
    let mut stdout = BufReader::new(
        child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("no supervisor stdout"))?,
    );
    send_message(
        &mut stdin,
        &HostMessage::Hello {
            proto: PROTOCOL_V2_VERSION,
            grant_class: "dev".into(),
        },
    )?;
    match recv_message(&mut stdout)? {
        GuestMessage::HelloAck { proto, .. } if proto == PROTOCOL_V2_VERSION => {}
        other => bail!("unexpected supervisor handshake: {other:?}"),
    }

    let audited = audit_ktool_bundle(bundle).context("auditing the generated dev bundle")?;
    send_message(
        &mut stdin,
        &HostMessage::LoadTool {
            tool_digest: audited.tool_digest.to_string(),
            runtime_image_digest: audited.manifest.runtime_image.digest.to_string(),
            bundle_len: bundle.len() as u64,
        },
    )?;
    write_frame(&mut stdin, bundle)?;
    match recv_message(&mut stdout)? {
        GuestMessage::LoadAck { ok: true, .. } => {}
        GuestMessage::LoadAck { error, .. } => bail!(
            "supervisor rejected the dev bundle: {}",
            error.unwrap_or_default()
        ),
        other => bail!("unexpected supervisor load reply: {other:?}"),
    }

    let deadline_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| anyhow!("host clock precedes the Unix epoch"))?
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
        + 10_000;
    send_message(
        &mut stdin,
        &HostMessage::Invoke(Box::new(InvokeRequest {
            request_id: 1,
            tool: audited.descriptor.name.clone(),
            tool_digest: audited.tool_digest.to_string(),
            call_context: CallContext {
                deadline_ms,
                ..Default::default()
            },
            input,
            deadline_ms,
        })),
    )?;
    loop {
        match recv_message(&mut stdout)? {
            GuestMessage::Event { .. } => continue,
            message @ GuestMessage::Result { .. } => return Ok(serde_json::to_value(message)?),
            other => bail!("unexpected supervisor message: {other:?}"),
        }
    }
}

fn send_message<W: Write>(writer: &mut W, message: &HostMessage) -> Result<()> {
    write_frame(writer, &serde_json::to_vec(message)?)
}

fn recv_message<R: Read>(reader: &mut R) -> Result<GuestMessage> {
    serde_json::from_slice(&read_frame(reader)?)
        .context("supervisor frame was not a v2 guest message")
}

fn write_frame<W: Write>(writer: &mut W, body: &[u8]) -> Result<()> {
    writer.write_all(&(body.len() as u32).to_be_bytes())?;
    writer.write_all(body)?;
    writer.flush()?;
    Ok(())
}

fn read_frame<R: Read>(reader: &mut R) -> Result<Vec<u8>> {
    let mut header = [0_u8; 4];
    reader
        .read_exact(&mut header)
        .context("reading a supervisor frame header")?;
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_FRAME_BYTES {
        bail!("supervisor frame of {len} bytes exceeds the {MAX_FRAME_BYTES}-byte cap");
    }
    let mut body = vec![0_u8; len];
    reader
        .read_exact(&mut body)
        .context("reading a supervisor frame body")?;
    Ok(body)
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
        if let Some(error) = result.get("error") {
            println!("  error:  {error}");
        }
    }
}

fn supervisor_binary() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("KARAKARA_SUPERVISOR_BIN") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        bail!(
            "KARAKARA_SUPERVISOR_BIN points at {}, which is not a file",
            path.display()
        );
    }
    if let Ok(current) = std::env::current_exe()
        && let Some(sibling) = current.parent().map(|dir| dir.join("baml-tool-supervisor"))
        && sibling.is_file()
    {
        return Ok(sibling);
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join("baml-tool-supervisor");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    bail!(
        "guest supervisor binary not found. Build it with `cargo build -p baml-tool-supervisor` or set KARAKARA_SUPERVISOR_BIN."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let body = br#"{"t":"invoke"}"#;
        let mut framed = Vec::new();
        write_frame(&mut framed, body).unwrap();
        assert_eq!(read_frame(&mut std::io::Cursor::new(framed)).unwrap(), body);
    }
}
