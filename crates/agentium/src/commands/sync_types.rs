// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! `agentium sync-types` — pull server-generated types after publish.

use anyhow::{Result, bail};
use console::style;

use crate::{
    commands::utils::{AgentPlatform, RunnerToken},
    project,
};

pub fn run(path: Option<&str>, runner_token: Option<RunnerToken>) -> Result<()> {
    let (cfg, _) = project::discover_config(None)?;
    let cwd = std::env::current_dir()?;
    let agent_dir = if let Some(p) = path {
        cwd.join(p)
    } else {
        cfg.default_agent_path(&cwd)
    };
    let agent = cfg
        .project
        .default_agent
        .clone()
        .unwrap_or_else(|| ".".to_string());
    let url = format!("{}/dev-artifacts?agent={agent}", cfg.repository_url());
    let http = AgentPlatform::new(runner_token)?;
    let resp: serde_json::Value = http.get_json(&url, "sync-types")?;
    if !matches!(resp.get("status").and_then(|v| v.as_str()), Some("ok")) {
        bail!(
            "Dev artifacts not available (status={}). Run `agentium install agent` after publish.",
            resp.get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
        );
    }
    if let Some(prelude) = resp.get("baml_runtime").and_then(|v| v.as_str()) {
        let out = agent_dir.join("baml_src/_baml_runtime.baml");
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&out, prelude)?;
        println!("{} Wrote {}", style("[sync-types]").green(), out.display());
    }
    if let Some(dts) = resp.get("baml_runtime_dts").and_then(|v| v.as_str()) {
        let out = agent_dir.join("src/baml-runtime.d.ts");
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&out, dts)?;
        println!("{} Wrote {}", style("[sync-types]").green(), out.display());
    }
    Ok(())
}
