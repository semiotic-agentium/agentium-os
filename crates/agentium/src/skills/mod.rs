// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Bundled Cursor skills for agent/tool authoring.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use console::style;

const AGENT_SKILL: &str = include_str!("../../skills/agentium-agent-authoring/SKILL.md");
const TOOL_SKILL: &str = include_str!("../../skills/agentium-tool-authoring/SKILL.md");

pub fn install(kind: &str, dest_root: Option<&Path>) -> Result<()> {
    let base = dest_root
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(".cursor/skills"));
    let (name, content) = match kind {
        "agent" => ("agentium-agent-authoring", AGENT_SKILL),
        "tool" => ("agentium-tool-authoring", TOOL_SKILL),
        other => bail!("Unknown skill kind: {other}. Use agent or tool."),
    };
    let dir = base.join(name);
    fs::create_dir_all(&dir)?;
    let path = dir.join("SKILL.md");
    fs::write(&path, content).with_context(|| format!("Failed to write {}", path.display()))?;
    println!(
        "{} Installed skill to {}",
        style("[skill]").green(),
        path.display()
    );
    Ok(())
}
