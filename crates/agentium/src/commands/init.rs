// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! `agentium init` — scaffold project config (and optional first agent).

use std::path::Path;

use anyhow::Result;
use console::style;

use crate::project;

pub fn run(dir: &Path, runner_url: &str, agent_name: Option<&str>, with_agent: bool) -> Result<()> {
    let config_path = project::init_project(dir, runner_url, agent_name)?;
    println!(
        "{} Created {}",
        style("[init]").bold().dim(),
        style(config_path.display()).cyan()
    );

    if with_agent {
        let name = agent_name.unwrap_or("my-agent");
        crate::commands::new_agent::run(
            name,
            None,
            "simple",
            "",
            None,
            None,
            Some(&dir.join(name).to_string_lossy()),
            false,
            false,
        )?;
    }

    println!(
        "\nNext: edit source, then `{}`",
        style("agentium install agent").cyan()
    );
    Ok(())
}
