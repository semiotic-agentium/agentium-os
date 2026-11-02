// SPDX-FileCopyrightText: 2026 Karakara Labs
// SPDX-License-Identifier: Apache-2.0

//! `karak tool build` asks an MCP server for `tools/list` and records the
//! resulting descriptors. Tool code owns MCP. Karakara only packages and audits
//! the server as a toolset.

use std::{
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, anyhow, bail};
use baml_rt_tools::external_tools::{UnifiedToolDescriptor, compute_descriptor_digest};
use console::style;
use serde_json::{Value, json};

use crate::project::{ToolsetConfig, load_toolset_config};

pub(crate) struct BuiltToolset {
    pub config: ToolsetConfig,
    pub descriptors: Vec<UnifiedToolDescriptor>,
}

pub fn run(dir: &str) -> Result<()> {
    let dir = Path::new(dir);
    let built = build_toolset(dir, true)?;
    let pretty = format!("{}\n", serde_json::to_string_pretty(&built.descriptors)?);
    let tools_path = dir.join("tools.json");
    std::fs::write(&tools_path, pretty)
        .with_context(|| format!("writing {}", tools_path.display()))?;
    write_descriptor_lock(dir, &built)?;

    println!(
        "{} built {}",
        style("✓").green(),
        style(dir.display()).cyan()
    );
    println!("  tools:      {}", style(tools_path.display()).cyan());
    for descriptor in &built.descriptors {
        println!(
            "  {}  {}",
            descriptor.name,
            compute_descriptor_digest(descriptor)
        );
    }
    Ok(())
}

/// Build the project, start its ordinary MCP stdio server, and normalize its
/// `tools/list` result into full Karakara descriptor names.
pub(crate) fn build_toolset(dir: &Path, force_rebuild: bool) -> Result<BuiltToolset> {
    if !dir.is_dir() {
        bail!("Error: not a directory: {}", dir.display());
    }
    let config = load_toolset_config(dir)?;
    if force_rebuild {
        rebuild(dir)?;
    } else {
        ensure_built(dir)?;
    }
    let listed = extract_tools_json(dir)?;
    let descriptors = finish_build(&listed, &config.name)?;
    Ok(BuiltToolset {
        config,
        descriptors,
    })
}

fn write_descriptor_lock(dir: &Path, built: &BuiltToolset) -> Result<()> {
    let lock = json!({
        "toolset": built.config.name,
        "version": built.config.version,
        "tools": built.descriptors.iter().map(|descriptor| json!({
            "name": descriptor.name,
            "descriptor_digest": compute_descriptor_digest(descriptor),
        })).collect::<Vec<_>>(),
    });
    let path = dir.join("tool.lock.json");
    std::fs::write(&path, format!("{}\n", serde_json::to_string_pretty(&lock)?))
        .with_context(|| format!("writing {}", path.display()))
}

/// Convert an MCP `tools/list` response to canonical, fully qualified tool
/// descriptors. The server exposes local names; a bundle owns the prefix.
pub fn finish_build(tools_json: &str, toolset_name: &str) -> Result<Vec<UnifiedToolDescriptor>> {
    let listed: Value =
        serde_json::from_str(tools_json).context("tools/list output was not JSON")?;
    let tools = listed
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("tools/list response did not contain a tools array"))?;
    if tools.is_empty() {
        bail!("tools/list returned no tools");
    }

    let mut descriptors = tools
        .iter()
        .map(|tool| {
            let mut descriptor = UnifiedToolDescriptor::from_mcp(tool)
                .map_err(|error| anyhow!("invalid MCP tool in tools/list: {error}"))?;
            if descriptor.name.is_empty() || descriptor.name.contains('/') {
                bail!(
                    "MCP tool name '{}' must be a non-empty local name without '/'; the toolset supplies '{} /'",
                    descriptor.name,
                    toolset_name
                );
            }
            descriptor.name = format!("{toolset_name}/{}", descriptor.name);
            Ok(descriptor)
        })
        .collect::<Result<Vec<_>>>()?;
    descriptors.sort_by(|left, right| left.name.cmp(&right.name));
    if descriptors
        .windows(2)
        .any(|pair| pair[0].name == pair[1].name)
    {
        bail!("tools/list returned duplicate tool names");
    }
    Ok(descriptors)
}

/// Compile the project if `dist/tool.js` is missing.
pub(crate) fn ensure_built(dir: &Path) -> Result<()> {
    if dir.join("dist/tool.js").is_file() {
        return Ok(());
    }
    rebuild(dir)
}

/// Recompile even when an older `dist/tool.js` exists. Publish and test use
/// this so their bundle always reflects the source tree.
pub(crate) fn rebuild(dir: &Path) -> Result<()> {
    if !dir.join("node_modules").is_dir() {
        bail!(
            "Error: dependencies not installed in {}.\nHint: run `npm install` first.",
            dir.display()
        );
    }
    let status = Command::new("npm")
        .args(["run", "build"])
        .current_dir(dir)
        .status()
        .with_context(|| "running `npm run build` (is node/npm on PATH?)")?;
    if !status.success() {
        bail!("Error: `npm run build` failed in {}", dir.display());
    }
    if !dir.join("dist/tool.js").is_file() {
        bail!(
            "Error: build did not produce dist/tool.js in {}.\nHint: check tsconfig outDir/rootDir.",
            dir.display()
        );
    }
    Ok(())
}

/// Run the built MCP server long enough to initialize it and request
/// `tools/list`. This deliberately talks JSON-RPC over MCP stdio instead of
/// importing a Karakara-specific entrypoint.
pub(crate) fn extract_tools_json(dir: &Path) -> Result<String> {
    let entry = dir
        .join("dist/tool.js")
        .canonicalize()
        .context("resolving dist/tool.js")?;
    let mut child = Command::new("node")
        .arg(&entry)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| "starting the MCP server with node")?;
    let result = (|| {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("MCP server stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("MCP server stdout unavailable"))?;
        let mut stdout = BufReader::new(stdout);

        write_jsonrpc(
            &mut stdin,
            &json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18", "capabilities": {},
                    "clientInfo": {"name": "karak", "version": env!("CARGO_PKG_VERSION")}
                }
            }),
        )?;
        read_response(&mut stdout, 1)?;
        write_jsonrpc(
            &mut stdin,
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}}),
        )?;
        write_jsonrpc(
            &mut stdin,
            &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
        )?;
        let response = read_response(&mut stdout, 2)?;
        serde_json::to_string(
            response
                .get("result")
                .ok_or_else(|| anyhow!("tools/list response had no result"))?,
        )
        .context("serializing tools/list result")
    })();
    let _ = child.kill();
    let output = child.wait_with_output().ok();
    result.map_err(|error| {
        let stderr = output
            .as_ref()
            .map(|output| String::from_utf8_lossy(&output.stderr).trim().to_string())
            .filter(|stderr| !stderr.is_empty());
        match stderr {
            Some(stderr) => anyhow!("{error}\nMCP server stderr: {stderr}"),
            None => error,
        }
    })
}

fn write_jsonrpc(writer: &mut impl Write, message: &Value) -> Result<()> {
    serde_json::to_writer(&mut *writer, message)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn read_response(reader: &mut impl BufRead, id: u64) -> Result<Value> {
    let mut line = String::new();
    loop {
        line.clear();
        let count = reader
            .read_line(&mut line)
            .context("reading MCP server output")?;
        if count == 0 {
            bail!("MCP server closed stdout before replying to request {id}");
        }
        if line.len() > baml_sandbox_protocol::MAX_FRAME_BYTES {
            bail!("MCP server response exceeds the output cap");
        }
        let value: Value = serde_json::from_str(line.trim())
            .context("MCP server wrote non-JSON data to stdout")?;
        if value.get("id").and_then(Value::as_u64) == Some(id) {
            if let Some(error) = value.get("error") {
                bail!("MCP request {id} failed: {error}");
            }
            return Ok(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_build_qualifies_and_sorts_mcp_tools() {
        let descriptors = finish_build(
            r#"{"tools":[
              {"name":"zebra","description":"z","inputSchema":{},"annotations":{"readOnlyHint":true}},
              {"name":"alpha","description":"a","inputSchema":{}}
            ]}"#,
            "support",
        )
        .unwrap();
        assert_eq!(descriptors[0].name, "support/alpha");
        assert_eq!(descriptors[1].name, "support/zebra");
    }

    #[test]
    fn finish_build_rejects_qualified_mcp_names() {
        assert!(finish_build(r#"{"tools":[{"name":"support/echo"}]}"#, "support").is_err());
    }
}
