// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! External-tool registry operator commands.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use baml_rt_tools::{
    approval::ApprovalState,
    external_tool_cache,
    external_tools::{
        ExternalToolSnapshot, ToolRuntime, discover_snapshot, now_snapshot_timestamp,
    },
};
use console::style;
use inquire::Confirm;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Arguments for [`enable`], grouped to keep the call site (and clippy) happy.
#[derive(Debug)]
pub struct EnableParams<'a> {
    pub dir: &'a str,
    pub repository_url: Option<&'a str>,
    pub runner_token: Option<&'a str>,
    pub sandbox_rootfs: Option<&'a str>,
    pub approved_by: Option<&'a str>,
    pub yes: bool,
    pub json_output: bool,
}

pub fn enable(params: EnableParams<'_>) -> Result<()> {
    let EnableParams {
        dir,
        repository_url,
        runner_token,
        sandbox_rootfs,
        approved_by,
        yes,
        json_output,
    } = params;

    let dir = PathBuf::from(dir);
    let Some(repository_url) = repository_url else {
        bail!("external-tool enable requires --repository-url");
    };

    if !approve(
        yes,
        "Ask runner to discover, approve, and import external tool snapshot?",
    )? {
        println!("approval declined; cache unchanged");
        return Ok(());
    }
    let body = enable_via_runner(
        &dir,
        repository_url,
        runner_token,
        sandbox_rootfs,
        approved_by,
    )?;
    if json_output {
        println!("{}", serde_json::to_string_pretty(&body)?);
    } else {
        print_snapshot_summary("External tool discovery", &body.snapshot);
        let version = body.version.version;
        println!(
            "imported {} into registry as version {version}",
            style(&body.snapshot.tool.name).cyan()
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
    repository_url: Option<&str>,
    runner_token: Option<&str>,
    yes: bool,
    json_output: bool,
) -> Result<()> {
    let Some(repository_url) = repository_url else {
        bail!("external-tool refresh requires --repository-url");
    };
    let old: Option<ExternalToolSnapshot> = None;
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
    post_to_registry(&approved, Some(repository_url), runner_token, json_output)?;
    if !json_output {
        println!(
            "approved {} snapshot={}",
            style(name).cyan(),
            approved.snapshot_digest
        );
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct EnableExternalToolRequest {
    tool_dir: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    sandbox_rootfs: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    approved_by: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct EnableExternalToolResponse {
    snapshot: ExternalToolSnapshot,
    version: baml_rt_repository::ExternalToolRegistryToolVersion,
}

fn enable_via_runner(
    dir: &Path,
    repository_url: &str,
    runner_token: Option<&str>,
    sandbox_rootfs: Option<&str>,
    approved_by: Option<&str>,
) -> Result<EnableExternalToolResponse> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let token = baml_rt_builder::external_tool_registry::resolve_runner_token(runner_token)?;
        let url = format!(
            "{}/external-tools/enable",
            repository_url.trim_end_matches('/')
        );
        let body = EnableExternalToolRequest {
            tool_dir: dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf()),
            sandbox_rootfs: sandbox_rootfs.map(PathBuf::from),
            approved_by: approved_by.map(str::to_string),
        };
        let http = reqwest::Client::new();
        let mut request = http.post(url.as_str()).json(&body);
        if let Some(ref token) = token {
            request = request.header("X-Runner-Token", token.as_str());
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("asking runner to enable external tool at {url}"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            bail!(
                "external-tool runner approval failed ({status}): {text}. Hint: pass --runner-token <token> or set RUNNER_TOKEN"
            );
        }
        if !status.is_success() {
            bail!("external-tool runner approval failed ({status}): {text}");
        }
        serde_json::from_str(&text).context("parsing external-tool enable response")
    })
}

/// Post an approved snapshot to the repository registry when `--repository-url`
/// is set. Cache writes already happened; the registry is an additional sink.
fn post_to_registry(
    snapshot: &ExternalToolSnapshot,
    repository_url: Option<&str>,
    runner_token: Option<&str>,
    json_output: bool,
) -> Result<()> {
    let Some(repository_url) = repository_url else {
        return Ok(());
    };
    let rt = tokio::runtime::Runtime::new()?;
    let body = rt.block_on(
        baml_rt_builder::external_tool_registry::post_external_tool_snapshot_to_registry(
            repository_url,
            snapshot.clone(),
            runner_token,
            "external-tool registry import",
        ),
    )?;
    if !json_output {
        let version = body
            .get("version")
            .and_then(|v| v.get("version"))
            .and_then(|v| v.as_u64())
            .map(|v| v.to_string())
            .unwrap_or_else(|| "<unknown>".into());
        println!(
            "imported {} into registry as version {version}",
            style(&snapshot.tool.name).cyan()
        );
    }
    Ok(())
}

fn discover_process_snapshot(dir: &Path) -> Result<ExternalToolSnapshot> {
    let rt = tokio::runtime::Runtime::new()?;
    Ok(rt.block_on(discover_snapshot(dir, None, None))?)
}

fn approved(mut snapshot: ExternalToolSnapshot) -> ExternalToolSnapshot {
    // Safe: snapshot digest excludes approval metadata.
    snapshot.approval.state = ApprovalState::Approved;
    snapshot.approval.reviewed_at = Some(now_snapshot_timestamp());
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
