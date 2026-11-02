// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
// SPDX-FileCopyrightText: 2026 Karakara Labs
//
// SPDX-License-Identifier: Apache-2.0

//! `karak tool test` — run one invocation through the real guest supervisor in
//! DevBackend mode (P4.10).
//!
//! The supervisor binary is spawned with `--dev`: same wire, same bundle
//! verify/unpack, same worker harness and deadline/output/cancel logic as prod,
//! with bwrap isolation instead of a microVM. The bundle is built from the
//! project exactly like `karak publish` (vendored `node_modules` included), so
//! the tool resolves `@karakara/tool` the same way it will in a VM.

use std::{
    collections::HashSet,
    fs,
    io::{BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use baml_rt_tools::external_tools::{
    BundleFile, UnifiedToolDescriptor, build_ktool_bundle, compute_descriptor_digest,
    default_node_runtime_image,
};
use baml_sandbox_protocol::MAX_FRAME_BYTES;
use baml_sandbox_protocol::protocol_v2::{
    GuestMessage, HostMessage, InvokeRequest, PROTOCOL_V2_VERSION,
};
use console::style;
use serde_json::{Value, json};

use super::tool_build;

pub fn run(dir: &str, input: Option<&str>) -> Result<()> {
    let dir = Path::new(dir);
    if !dir.is_dir() {
        bail!("Error: not a directory: {}", dir.display());
    }
    // Rebuild from source so `karak tool test` never exercises a stale
    // dist/tool.js, matching `karak publish`.
    tool_build::rebuild(dir)?;

    let descriptor_json = tool_build::extract_descriptor_json(dir)?;
    let descriptor: UnifiedToolDescriptor = serde_json::from_str(&descriptor_json)
        .context("the SDK descriptor did not match the canonical ToolDescriptor")?;
    let bundle = build_dev_bundle(dir, &descriptor)?;

    let input_value: Value = match input {
        Some(raw) => serde_json::from_str(raw)
            .with_context(|| format!("--input is not valid JSON: {raw}"))?,
        None => json!({}),
    };

    let result = run_once(&bundle, input_value)?;
    print_result(&result);
    Ok(())
}

/// A minimal `.ktool`: the same metadata contract as publish, with the compiled
/// entrypoint and vendored dependencies so bare imports resolve in the sandbox.
fn build_dev_bundle(dir: &Path, descriptor: &UnifiedToolDescriptor) -> Result<Vec<u8>> {
    let entry = fs::read(dir.join("dist/tool.js")).context("reading dist/tool.js")?;
    let mut extra = vec![BundleFile::new(
        "code/package.json",
        br#"{"private":true,"type":"module"}"#.to_vec(),
    )];
    collect_node_modules(dir, &mut extra)?;
    let sbom = serde_json::to_vec(&json!({
        "descriptor_digest": compute_descriptor_digest(descriptor),
        "runtime_image": default_node_runtime_image(),
    }))?;
    build_ktool_bundle(descriptor, entry, extra, sbom)
        .map_err(|error| anyhow!("cannot build the dev .ktool bundle: {error}"))
}

fn collect_node_modules(project: &Path, files: &mut Vec<BundleFile>) -> Result<()> {
    let root = project.join("node_modules");
    if !root.is_dir() {
        return Ok(());
    }
    let mut active = HashSet::new();
    walk_tree(&root, "code/node_modules", files, &mut active)
}

fn walk_tree(
    actual: &Path,
    logical: &str,
    files: &mut Vec<BundleFile>,
    active: &mut HashSet<PathBuf>,
) -> Result<()> {
    let metadata = fs::metadata(actual)
        .with_context(|| format!("reading vendored dependency {}", actual.display()))?;
    if metadata.is_file() {
        files.push(BundleFile::new(
            logical,
            fs::read(actual).with_context(|| format!("reading {}", actual.display()))?,
        ));
        return Ok(());
    }
    if !metadata.is_dir() {
        bail!(
            "unsupported file type in node_modules: {}",
            actual.display()
        );
    }
    let canonical = actual
        .canonicalize()
        .with_context(|| format!("resolving {}", actual.display()))?;
    if !active.insert(canonical.clone()) {
        bail!("symlink cycle while vendoring {}", actual.display());
    }
    let mut entries = fs::read_dir(actual)
        .with_context(|| format!("reading {}", actual.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        walk_tree(
            &entry.path(),
            &format!("{logical}/{}", entry.file_name().to_string_lossy()),
            files,
            active,
        )?;
    }
    active.remove(&canonical);
    Ok(())
}

/// Spawn the supervisor, load the bundle over stdio, run one invoke.
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
    // `drive` drops stdin, so the supervisor sees EOF, kills its idle workers,
    // removes its dev root, and exits. Give it a moment before escalating.
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

    // The audit is deterministic; the supervisor re-verifies these exact bytes.
    let audited = baml_rt_tools::external_tools::audit_ktool_bundle(bundle)
        .context("auditing the generated dev bundle")?;
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
        GuestMessage::LoadAck { error, .. } => {
            bail!(
                "supervisor rejected the dev bundle: {}",
                error.unwrap_or_default()
            )
        }
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
            tool_digest: audited.tool_digest.to_string(),
            call_context: Default::default(),
            input,
            deadline_ms,
        })),
    )?;
    loop {
        let message = recv_message(&mut stdout)?;
        match message {
            GuestMessage::Event { .. } => continue,
            GuestMessage::Result { .. } => return Ok(serde_json::to_value(&message)?),
            other => bail!("unexpected supervisor message: {other:?}"),
        }
    }
}

fn send_message<W: Write>(writer: &mut W, message: &HostMessage) -> Result<()> {
    let value = serde_json::to_vec(message)?;
    write_frame(writer, &value)
}

fn recv_message<R: Read>(reader: &mut R) -> Result<GuestMessage> {
    let body = read_frame(reader)?;
    serde_json::from_slice(&body).context("supervisor frame was not a v2 guest message")
}

fn write_frame<W: Write>(writer: &mut W, body: &[u8]) -> Result<()> {
    writer.write_all(&(body.len() as u32).to_be_bytes())?;
    writer.write_all(body)?;
    writer.flush()?;
    Ok(())
}

fn read_frame<R: Read>(reader: &mut R) -> Result<Vec<u8>> {
    let mut header = [0u8; 4];
    reader
        .read_exact(&mut header)
        .context("reading a supervisor frame header")?;
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_FRAME_BYTES {
        bail!("supervisor frame of {len} bytes exceeds the {MAX_FRAME_BYTES}-byte cap");
    }
    let mut body = vec![0u8; len];
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
        if let Some(err) = result.get("error") {
            println!("  error:  {err}");
        }
    }
}

/// Locate the supervisor binary: explicit override, sibling of `karak`, then
/// `PATH`.
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
        "guest supervisor binary not found. Build it with \
         `cargo build -p baml-tool-supervisor` or set KARAKARA_SUPERVISOR_BIN."
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
        let mut cursor = std::io::Cursor::new(framed);
        assert_eq!(read_frame(&mut cursor).unwrap(), body);
    }

    #[test]
    fn walk_vendors_symlinked_packages() {
        let root = std::env::temp_dir().join(format!("karak-tool-test-{}", std::process::id()));
        let package = root.join("real-pkg");
        fs::create_dir_all(&package).unwrap();
        fs::write(package.join("index.js"), b"export default 1;").unwrap();
        let modules = root.join("app/node_modules/@scope");
        fs::create_dir_all(&modules).unwrap();
        std::os::unix::fs::symlink(&package, modules.join("pkg")).unwrap();

        let mut files = Vec::new();
        collect_node_modules(&root.join("app"), &mut files).unwrap();
        let paths: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();
        assert!(paths.contains(&"code/node_modules/@scope/pkg/index.js"));
        let _ = fs::remove_dir_all(root);
    }
}
