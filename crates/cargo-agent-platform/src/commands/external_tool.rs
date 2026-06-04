// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! External-tool snapshot cache operator commands.

use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use baml_rt_tools::{
    ToolName,
    approval::ApprovalState,
    external_tool_cache,
    external_tools::{
        ExternalInvoker, ExternalToolSnapshot, METHOD_INVOKE, PROTOCOL_VERSION,
        StdioSubprocessInvoker, ToolDescribeResult, ToolRuntime, read_external_manifest,
        validate_describe_schema_support,
    },
};
use console::style;
use inquire::Confirm;
use serde_json::json;

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(30);

pub fn enable(dir: &str, cache_dir: Option<&str>, yes: bool, json_output: bool) -> Result<()> {
    let dir = PathBuf::from(dir);
    let cache_root = cache_root(cache_dir)?;
    let snapshot = discover_process_snapshot(&dir)?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
    } else {
        print_snapshot_summary("External tool discovery", &snapshot);
    }

    if !approve(yes, "Approve external tool snapshot?")? {
        println!("approval declined; cache unchanged");
        return Ok(());
    }

    let approved = approved(snapshot);
    external_tool_cache::write_approved_snapshot(&cache_root, &approved)
        .with_context(|| format!("writing approved snapshot to {}", cache_root.display()))?;
    if !json_output {
        println!(
            "approved {} snapshot={}",
            style(&approved.tool.name).cyan(),
            approved.snapshot_digest
        );
    }
    Ok(())
}

pub fn inspect(name: &str, cache_dir: Option<&str>, json_output: bool) -> Result<()> {
    let cache_root = cache_root(cache_dir)?;
    let mut snapshots = Vec::new();

    if let Ok(path) = external_tool_cache::approved_snapshot_path(&cache_root, name)
        && path.exists()
    {
        snapshots.push(external_tool_cache::read_snapshot(&path)?);
    }
    for snap in external_tool_cache::read_pending_snapshots(&cache_root)? {
        if snap.tool.name == name {
            snapshots.push(snap);
        }
    }

    if snapshots.is_empty() {
        bail!(
            "no external tool snapshots found for '{name}' in {}",
            cache_root.display()
        );
    }

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "snapshots": snapshots }))?
        );
        return Ok(());
    }

    println!("{} {}", style("External tool:").bold(), style(name).cyan());
    for snap in snapshots {
        println!(
            "  state={:?} snapshot={} schema={} runtime={} created_at={}",
            snap.approval.state,
            snap.snapshot_digest,
            snap.digests.schema_digest,
            snap.digests.runtime_digest,
            snap.created_at,
        );
        println!("    manifest={}", snap.digests.manifest_digest);
    }
    Ok(())
}

pub fn refresh(
    name: &str,
    dir: &str,
    cache_dir: Option<&str>,
    yes: bool,
    json_output: bool,
) -> Result<()> {
    let cache_root = cache_root(cache_dir)?;
    let approved_path = external_tool_cache::approved_snapshot_path(&cache_root, name)?;
    let old = if approved_path.exists() {
        Some(external_tool_cache::read_snapshot(&approved_path)?)
    } else {
        None
    };
    let new_snapshot = discover_process_snapshot(&PathBuf::from(dir))?;
    if new_snapshot.tool.name != name {
        bail!(
            "discovered '{}' from {} but refresh target is '{}'",
            new_snapshot.tool.name,
            dir,
            name
        );
    }

    if let Some(old) = &old
        && old.snapshot_digest == new_snapshot.snapshot_digest
    {
        if json_output {
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &json!({ "status": "unchanged", "snapshot": new_snapshot })
                )?
            );
        } else {
            println!(
                "unchanged {} snapshot={}",
                style(name).cyan(),
                old.snapshot_digest
            );
        }
        return Ok(());
    }

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "status": "changed",
                "old": old,
                "new": new_snapshot,
            }))?
        );
    } else {
        print_refresh_diff(old.as_ref(), &new_snapshot);
    }

    if !approve(yes, "Approve refreshed external tool snapshot?")? {
        println!("approval declined; cache unchanged");
        return Ok(());
    }

    let approved = approved(new_snapshot);
    external_tool_cache::write_approved_snapshot(&cache_root, &approved)
        .with_context(|| format!("writing approved snapshot to {}", cache_root.display()))?;
    if !json_output {
        println!(
            "approved {} snapshot={}",
            style(name).cyan(),
            approved.snapshot_digest
        );
    }
    Ok(())
}

fn discover_process_snapshot(dir: &Path) -> Result<ExternalToolSnapshot> {
    let manifest = read_external_manifest(dir)
        .with_context(|| format!("reading external tool manifest from {}", dir.display()))?;
    let tool_name = ToolName::parse(&manifest.name)?;
    let invoker = process_invoker(dir, manifest.runtime.as_ref())?;
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let describe = invoker.describe(&tool_name, DISCOVERY_TIMEOUT).await?;
        if describe.protocol_version != PROTOCOL_VERSION {
            bail!(
                "tool/describe protocol_version '{}' but expected '{}'",
                describe.protocol_version,
                PROTOCOL_VERSION
            );
        }
        let describe_result: ToolDescribeResult = describe.into();
        if !describe_result
            .supported_methods
            .iter()
            .any(|m| m == METHOD_INVOKE)
        {
            bail!(
                "tool '{}' does not advertise {}",
                manifest.name,
                METHOD_INVOKE
            );
        }
        let describe_snapshot = validate_describe_schema_support(&manifest.name, &describe_result)?;
        let schema = invoker.schema(&tool_name, DISCOVERY_TIMEOUT).await?;
        ExternalToolSnapshot::from_parts(dir, manifest, schema, describe_snapshot, now_string())
            .map_err(Into::into)
    })
}

fn process_invoker(dir: &Path, runtime: Option<&ToolRuntime>) -> Result<StdioSubprocessInvoker> {
    let command = match runtime.cloned().unwrap_or_default() {
        ToolRuntime::Process(spec) => spec.command,
        ToolRuntime::Sandbox(_) => {
            bail!("external-tool CLI cache discovery supports process runtime only")
        }
    };
    let mut command = if command.is_empty() {
        vec![baml_rt_tools::external_tools::DEFAULT_PROCESS_COMMAND.to_string()]
    } else {
        command
    };
    if let Some(first) = command.first_mut() {
        let path = PathBuf::from(&first);
        if path.is_relative() {
            *first = dir.join(path).to_string_lossy().to_string();
        }
    }
    Ok(StdioSubprocessInvoker::from_command(command)?.with_working_dir(dir.to_path_buf()))
}

fn approved(mut snapshot: ExternalToolSnapshot) -> ExternalToolSnapshot {
    // Safe: snapshot digest excludes approval metadata.
    snapshot.approval.state = ApprovalState::Approved;
    snapshot.approval.reviewed_at = Some(now_string());
    snapshot
}

fn approve(yes: bool, prompt: &str) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    Ok(Confirm::new(prompt).with_default(false).prompt()?)
}

fn cache_root(cache_dir: Option<&str>) -> Result<PathBuf> {
    if let Some(dir) = cache_dir {
        return Ok(PathBuf::from(dir));
    }
    if let Ok(dir) = std::env::var("BAML_EXTERNAL_TOOL_CACHE_DIR") {
        return Ok(PathBuf::from(dir));
    }
    Ok(std::env::current_dir()?.join(".baml-external-tool-cache"))
}

fn print_snapshot_summary(title: &str, snap: &ExternalToolSnapshot) {
    println!("{} {}", style(title).bold(), style(&snap.tool.name).cyan());
    println!("  description: {}", snap.tool.description);
    println!("  access: {:?}", snap.tool.access_level);
    println!("  runtime: {}", runtime_kind(snap.tool.runtime.as_ref()));
    println!("  secrets: {}", snap.tool.secrets.join(","));
    println!("  schema_digest: {}", snap.digests.schema_digest);
    println!("  manifest_digest: {}", snap.digests.manifest_digest);
    println!("  runtime_digest: {}", snap.digests.runtime_digest);
    println!("  snapshot_digest: {}", snap.snapshot_digest);
    println!("  input_schema: {}", compact(&snap.tool.schemas.input));
    println!("  output_schema: {}", compact(&snap.tool.schemas.output));
}

fn print_refresh_diff(old: Option<&ExternalToolSnapshot>, new: &ExternalToolSnapshot) {
    println!(
        "{} {}",
        style("External tool refresh:").bold(),
        style(&new.tool.name).cyan()
    );
    match old {
        Some(old) => {
            print_digest_change(
                "schema",
                &old.digests.schema_digest.to_string(),
                &new.digests.schema_digest.to_string(),
            );
            print_digest_change(
                "manifest",
                &old.digests.manifest_digest.to_string(),
                &new.digests.manifest_digest.to_string(),
            );
            print_digest_change(
                "runtime",
                &old.digests.runtime_digest.to_string(),
                &new.digests.runtime_digest.to_string(),
            );
            print_digest_change(
                "snapshot",
                &old.snapshot_digest.to_string(),
                &new.snapshot_digest.to_string(),
            );
        }
        None => println!("  no approved snapshot exists"),
    }
    println!("  input_schema: {}", compact(&new.tool.schemas.input));
    println!("  output_schema: {}", compact(&new.tool.schemas.output));
}

fn print_digest_change(label: &str, old: &str, new: &str) {
    let marker = if old == new { "=" } else { "!" };
    println!("  {label}: {marker} {old} -> {new}");
}

fn runtime_kind(runtime: Option<&ToolRuntime>) -> &'static str {
    match runtime {
        Some(ToolRuntime::Sandbox(_)) => "sandbox",
        _ => "process",
    }
}

fn compact(value: &serde_json::Value) -> String {
    let s = serde_json::to_string(value).unwrap_or_else(|_| "<invalid-json>".to_string());
    if s.len() > 240 {
        format!("{}…", &s[..240])
    } else {
        s
    }
}

fn now_string() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format_unix_rfc3339_utc(secs)
}

fn format_unix_rfc3339_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let seconds_of_day = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn civil_from_days(days_since_unix_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_unix_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    let year = year + if month <= 2 { 1 } else { 0 };
    (year, month as u32, day as u32)
}
